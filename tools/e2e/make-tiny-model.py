#!/usr/bin/env python3
"""Create a tiny random-weight LLaMA-architecture GGUF model for plumbing tests.

The model knows nothing, but llama.cpp loads and runs it like any other. Under
C.O.R.E.'s grammar it can still only emit well-formed intents, which is exactly
what the end-to-end test needs to prove: llama-server accepts the generated
grammar, constrains sampling with it, and the agent parses what comes back.

The tokenizer is copied from one of llama.cpp's vocab-only test files.

    PYTHONPATH=llama.cpp/gguf-py python3 make-tiny-model.py \
        --vocab llama.cpp/models/ggml-vocab-llama-spm.gguf --out tiny.gguf
"""

import argparse

import numpy as np
from gguf import GGUFReader, GGUFValueType, GGUFWriter

CHATML = (
    "{% for message in messages %}"
    "{{ '<|im_start|>' + message['role'] + '\n' + message['content'] + '<|im_end|>' + '\n' }}"
    "{% endfor %}"
    "{% if add_generation_prompt %}{{ '<|im_start|>assistant\n' }}{% endif %}"
)


def copy_tokenizer(reader: GGUFReader, writer: GGUFWriter) -> int:
    """Copy every tokenizer.* key; return the vocabulary size."""
    vocab_size = 0
    for field in reader.fields.values():
        if not field.name.startswith("tokenizer.") or field.name == "tokenizer.chat_template":
            continue
        vtype = field.types[0]
        value = field.contents()
        if vtype == GGUFValueType.ARRAY:
            writer.add_key_value(field.name, value, vtype, sub_type=field.types[-1])
            if field.name == "tokenizer.ggml.tokens":
                vocab_size = len(value)
        else:
            writer.add_key_value(field.name, value, vtype)
    if vocab_size == 0:
        raise SystemExit("vocab file has no tokenizer.ggml.tokens")
    return vocab_size


def main() -> None:
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--vocab", required=True, help="vocab-only GGUF to take the tokenizer from")
    ap.add_argument("--out", required=True)
    ap.add_argument("--embd", type=int, default=64)
    ap.add_argument("--layers", type=int, default=2)
    ap.add_argument("--heads", type=int, default=4)
    ap.add_argument("--ff", type=int, default=128)
    ap.add_argument("--ctx", type=int, default=8192)
    ap.add_argument("--seed", type=int, default=7)
    args = ap.parse_args()

    rng = np.random.default_rng(args.seed)
    reader = GGUFReader(args.vocab)
    writer = GGUFWriter(args.out, "llama")
    writer.add_name("core-tiny-test")
    writer.add_context_length(args.ctx)
    writer.add_embedding_length(args.embd)
    writer.add_block_count(args.layers)
    writer.add_feed_forward_length(args.ff)
    writer.add_head_count(args.heads)
    writer.add_head_count_kv(args.heads)
    writer.add_layer_norm_rms_eps(1e-5)
    writer.add_rope_dimension_count(args.embd // args.heads)
    n_vocab = copy_tokenizer(reader, writer)
    writer.add_chat_template(CHATML)

    def w(*shape: int) -> np.ndarray:
        return (rng.standard_normal(shape) * 0.02).astype(np.float32)

    ones = lambda n: np.ones(n, dtype=np.float32)  # noqa: E731

    # numpy shapes are (rows, cols); GGUF stores them reversed, matching llama.cpp's
    # expectations for weight matrices of shape {n_in, n_out}.
    writer.add_tensor("token_embd.weight", w(n_vocab, args.embd))
    writer.add_tensor("output_norm.weight", ones(args.embd))
    writer.add_tensor("output.weight", w(n_vocab, args.embd))
    for i in range(args.layers):
        p = f"blk.{i}"
        writer.add_tensor(f"{p}.attn_norm.weight", ones(args.embd))
        writer.add_tensor(f"{p}.attn_q.weight", w(args.embd, args.embd))
        writer.add_tensor(f"{p}.attn_k.weight", w(args.embd, args.embd))
        writer.add_tensor(f"{p}.attn_v.weight", w(args.embd, args.embd))
        writer.add_tensor(f"{p}.attn_output.weight", w(args.embd, args.embd))
        writer.add_tensor(f"{p}.ffn_norm.weight", ones(args.embd))
        writer.add_tensor(f"{p}.ffn_gate.weight", w(args.ff, args.embd))
        writer.add_tensor(f"{p}.ffn_up.weight", w(args.ff, args.embd))
        writer.add_tensor(f"{p}.ffn_down.weight", w(args.embd, args.ff))

    writer.write_header_to_file()
    writer.write_kv_data_to_file()
    writer.write_tensors_to_file()
    writer.close()
    print(f"wrote {args.out}: vocab {n_vocab}, {args.layers} layers, embd {args.embd}")


if __name__ == "__main__":
    main()
