// gbnf-check: verify a GBNF grammar with llama.cpp's own grammar engine.
//
// Usage: gbnf-check GRAMMAR_FILE CASES_FILE
//
// CASES_FILE holds one case per line: "+ <text>" must be accepted in full by the
// grammar, "- <text>" must be rejected. Exits non-zero on any parse error or
// mismatch. Built against llama.cpp internals (see build.sh); no model needed.

#include "llama-grammar.h"
#include "unicode.h"

#include <cstdio>
#include <fstream>
#include <sstream>
#include <string>

static std::string slurp(const char * path) {
    std::ifstream in(path, std::ios::binary);
    if (!in) {
        fprintf(stderr, "cannot read %s\n", path);
        exit(2);
    }
    std::stringstream ss;
    ss << in.rdbuf();
    return ss.str();
}

static bool matches(const std::string & grammar_text, const std::string & input) {
    llama_grammar * grammar =
        llama_grammar_init_impl(nullptr, grammar_text.c_str(), "root", false, nullptr, 0, nullptr, 0);
    if (grammar == nullptr) {
        return false;
    }
    bool ok = true;
    size_t offset = 0;
    const auto & stacks = llama_grammar_get_stacks(grammar);
    while (offset < input.size() && ok) {
        uint32_t cpt = unicode_cpt_from_utf8(input, offset);
        try {
            llama_grammar_accept_str(*grammar, unicode_cpt_to_utf8(cpt));
        } catch (const std::exception &) {
            ok = false;
        }
        if (stacks.empty()) {
            ok = false;
        }
    }
    bool complete = false;
    if (ok) {
        for (const auto & stack : stacks) {
            if (stack.empty()) {
                complete = true;
            }
        }
    }
    llama_grammar_free_impl(grammar);
    return ok && complete;
}

int main(int argc, char ** argv) {
    if (argc != 3) {
        fprintf(stderr, "usage: %s GRAMMAR_FILE CASES_FILE\n", argv[0]);
        return 2;
    }
    const std::string grammar_text = slurp(argv[1]);
    llama_grammar * probe = llama_grammar_init_impl(nullptr, grammar_text.c_str(), "root", false, nullptr, 0, nullptr, 0);
    if (probe == nullptr) {
        fprintf(stderr, "FAIL: llama.cpp could not parse the grammar\n");
        return 1;
    }
    llama_grammar_free_impl(probe);

    std::istringstream cases(slurp(argv[2]));
    std::string line;
    int failures = 0, total = 0;
    while (std::getline(cases, line)) {
        if (line.size() < 2 || (line[0] != '+' && line[0] != '-')) {
            continue;
        }
        const bool expect = line[0] == '+';
        const std::string input = line.substr(2);
        const bool got = matches(grammar_text, input);
        total++;
        if (got != expect) {
            failures++;
            fprintf(stderr, "FAIL (expected %s): %s\n", expect ? "accept" : "reject", input.c_str());
        }
    }
    printf("gbnf-check: %d/%d cases passed\n", total - failures, total);
    return failures == 0 ? 0 : 1;
}
