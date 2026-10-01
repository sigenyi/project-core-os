# Models

C.O.R.E. is offline-first: the operating system's management layer never calls a
cloud API, because it has to be able to repair the network when there is no network.
Two local models are used:

| Role | Runtime | Default | Size | Resident RAM |
|---|---|---|---|---|
| Reasoning (`core.gguf`) | llama.cpp `llama-server` | Qwen3-4B-Instruct-2507, Q4_K_M | ~2.5 GB | ~3.5 GB with 8K context |
| Speech-to-text (`whisper.bin`) | whisper.cpp `whisper-cli` | ggml-base (multilingual) | ~150 MB | only while transcribing |

Both are listed in [`image/models.conf`](../image/models.conf) and fetched with
`image/scripts/fetch-models.sh`, which verifies SHA-256 checksums. **The checksums
in the manifest are not pinned yet.** The first fetch prints them; pin them before
publishing an image.

## Choosing a reasoning model

The model must follow a long system prompt, emit structured tool calls and
recover from errors, all at 4-8B parameters. Good candidates in GGUF form:

| Model | Params | Q4_K_M size | Notes |
|---|---|---|---|
| Qwen3-4B-Instruct-2507 | 4B | ~2.5 GB | default; strong tool use for its size |
| Qwen3-8B (non-thinking mode) | 8B | ~5 GB | better diagnosis; needs ~6.5 GB RAM |
| Llama 3.1 8B Instruct | 8B | ~4.9 GB | solid general instruction following |
| Gemma 3 4B IT | 4B | ~2.5 GB | good multilingual quality |
| Qwen2.5-3B / Llama 3.2 3B | 3B | ~2 GB | low-RAM machines (4 GB total) |

The grammar guarantees well-formed output whatever the model, so choose for
judgement (diagnosis, picking the right action), not for JSON discipline. The agent
disables "thinking" mode on hybrid models (`enable_thinking=false`) because the
grammar constrains output from the first token.

### Sizing

Rough RAM needs = model file size + KV cache (≈ 0.1-0.3 GB per 1K tokens of context
for 4-8B models) + ~0.5 GB for the OS. With 8 GB of RAM a 4B model at 8K context is
comfortable. With 16 GB, an 8B model fits easily.

## Switching models

```sh
cp MyModel-Q4_K_M.gguf /usr/share/core/models/
ln -sfn MyModel-Q4_K_M.gguf /usr/share/core/models/core.gguf
systemctl restart core-inference
core-ctl doctor
```

If you change the context size (`CORE_CTX_SIZE` in `/etc/core/inference.env`), set
`[inference] context_tokens` in `/etc/core/agent.toml` to the same value. The agent
budgets prompts against it.

## GPU offload

Build llama.cpp with `--vulkan` (`image/build-iso.sh --vulkan`) and set
`CORE_GPU_LAYERS=99` in `/etc/core/inference.env`. Vulkan covers AMD, Intel and
NVIDIA GPUs through Mesa or the vendor drivers. The service already allows access
to DRM render nodes. ROCm or the proprietary NVIDIA stack need a drop-in adding
their device nodes (`DeviceAllow=`).

## Voice

Voice input is push-to-talk: `/voice` in the shell records with `arecord` (16 kHz
mono) until Enter is pressed, then transcribes locally. By default `whisper-cli`
loads the model only for each utterance, so no RAM is held while idle. For lower
latency, enable `core-whisper.service` and set `voice.transcriber =
"whisper-server"` in `agent.toml`.

## Testing without a real model

`tools/e2e/run.sh` synthesises a 17 MB random-weight model with llama.cpp's
`gguf-py`. It produces nonsense, but grammar-constrained nonsense: it exercises the
whole serving path (llama-server, grammar, parsing, the control loop, confirmations)
in seconds. In practice it regularly "decides" to power the machine off, which makes
a good demonstration of why confirmations exist.
