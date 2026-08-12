#define _POSIX_C_SOURCE 200809L

#include "policy.h"

#include <ctype.h>
#include <stdio.h>
#include <string.h>

typedef struct {
    bool in_block_comment;
    bool in_string;
    bool in_char;
} ScanState;

static const char *skip_space(const char *s) {
    while (*s && isspace((unsigned char)*s))
        s++;
    return s;
}

static bool path_matches(const char *path, const char *needle) {
    if (!path || !needle) return false;
    if (strcmp(path, needle) == 0) return true;
    size_t lp = strlen(path);
    size_t ln = strlen(needle);
    if (lp <= ln) return false;
    return (path[lp - ln - 1] == '/' && strcmp(path + lp - ln, needle) == 0);
}

static bool is_getenv_allowed_in_file(const LintConfig *cfg, const char *path) {
    if (cfg->allow_getenv_files.count == 0) return false;
    for (size_t i = 0; i < cfg->allow_getenv_files.count; i++) {
        if (path_matches(path, cfg->allow_getenv_files.items[i])) return true;
    }
    return false;
}

static bool is_stdio_allowed_in_file(const LintConfig *cfg, const char *path) {
    for (size_t i = 0; i < cfg->allow_stdio_files.count; i++) {
        if (path_matches(path, cfg->allow_stdio_files.items[i])) return true;
    }
    return false;
}

static bool is_shell_allowed_in_file(const LintConfig *cfg, const char *path) {
    for (size_t i = 0; i < cfg->allow_shell_files.count; i++) {
        if (path_matches(path, cfg->allow_shell_files.items[i])) return true;
    }
    return false;
}

static void emit_diag(const char *severity, const char *path, int line, int col, const char *rule,
                      const char *message) {
    if (line > 0) {
        fprintf(stdout, "%s:%d:%d: [%s] %s: %s\n", path, line, col, severity, rule, message);
    } else {
        fprintf(stdout, "%s: [%s] %s: %s\n", path, severity, rule, message);
    }
}

static bool word_starts_token(const char *line, const char *found, size_t len) {
    size_t offset = (size_t)(found - line);
    bool left_ok = offset == 0 || !(isalnum((unsigned char)line[offset - 1]) || line[offset - 1] == '_');
    bool right_ok = !isalnum((unsigned char)found[len]) && found[len] != '_';
    return left_ok && right_ok;
}

static bool line_has_word(const char *line, const char *word) {
    size_t wl = strlen(word);
    const char *p = line;
    while (true) {
        const char *m = strstr(p, word);
        if (!m) return false;
        if (word_starts_token(p, m, wl)) return true;
        p = m + wl;
    }
}

static bool line_has_call(const char *line, const char *name) {
    size_t length = strlen(name);
    const char *cursor = line;
    while (true) {
        const char *found = strstr(cursor, name);
        if (found == NULL) return false;
        if (word_starts_token(line, found, length)) {
            const char *next = found + length;
            while (isspace((unsigned char)*next))
                ++next;
            if (*next == '(') return true;
        }
        cursor = found + length;
    }
}

static bool line_has_direct_console_output(const char *line) {
    if (line_has_call(line, "printf") || line_has_call(line, "vprintf") || line_has_call(line, "puts") ||
        line_has_call(line, "putchar") || line_has_call(line, "perror")) {
        return true;
    }
    if (line_has_call(line, "dprintf") || line_has_call(line, "vdprintf")) return true;
    if ((line_has_call(line, "fprintf") || line_has_call(line, "vfprintf") || line_has_call(line, "fputs") ||
         line_has_call(line, "fputc")) &&
        (line_has_word(line, "stderr") || line_has_word(line, "stdout"))) {
        return true;
    }
    return false;
}

static bool line_has_direct_environment_access(const char *line) {
    static const char *const calls[] = {"getenv",    "secure_getenv",           "__secure_getenv",
                                        "_dupenv_s", "GetEnvironmentVariableA", "GetEnvironmentVariableW",
                                        NULL};
    for (size_t index = 0; calls[index] != NULL; ++index)
        if (line_has_call(line, calls[index])) return true;
    return false;
}

static bool line_has_platform_debug_output(const char *line) {
    static const char *const calls[] = {"OutputDebugStringA", "OutputDebugStringW", "NSLog", "os_log", "syslog", NULL};
    for (size_t index = 0; calls[index] != NULL; ++index)
        if (line_has_call(line, calls[index])) return true;
    return false;
}

static bool line_has_control_prefix(const char *line) {
    const char *s = skip_space(line);
    static const char *const k_controls[] = {"if",      "else",    "for",    "while",  "switch",       "case",
                                             "default", "do",      "goto",   "sizeof", "struct",       "union",
                                             "enum",    "typedef", "return", "asm",    "asm volatile", NULL};
    for (size_t i = 0; k_controls[i]; i++) {
        const char *kw = k_controls[i];
        size_t klen = strlen(kw);
        if (strncmp(s, kw, klen) == 0 && (isspace((unsigned char)s[klen]) || s[klen] == '(' || s[klen] == '\0')) {
            return true;
        }
    }
    return false;
}

static bool looks_like_function_signature(const char *sig) {
    if (!sig) return false;
    const char *s = skip_space(sig);
    if (!*s || *s == '#') return false;
    if (!strchr(s, '(') || !strchr(s, ')')) return false;
    if (strchr(s, ';')) return false;
    if (line_has_control_prefix(s)) return false;
    if (strstr(s, "static_assert") || strstr(s, "typedef")) return false;
    if (strstr(s, "sizeof(")) return false;
    if (strstr(s, "alignas(")) return false;
    return true;
}

static void strip_trailing_newline(char *line) {
    size_t len = strlen(line);
    if (len == 0) return;
    if (line[len - 1] == '\n') line[len - 1] = '\0';
}

static void sanitize_for_parse(const char *src, char *dst, size_t dst_len, ScanState *state) {
    size_t j = 0;
    bool in_line_comment = false;
    for (size_t i = 0; i < strlen(src) && j + 1 < dst_len; i++) {
        char c = src[i];
        char cnext = src[i + 1];
        if (in_line_comment) break;

        if (state->in_block_comment) {
            if (c == '*' && cnext == '/') {
                state->in_block_comment = false;
                i++;
            }
            continue;
        }
        if (state->in_string) {
            if (c == '\\' && cnext != '\0') {
                i++;
                continue;
            }
            if (c == '\"') state->in_string = false;
            continue;
        }
        if (state->in_char) {
            if (c == '\\' && cnext != '\0') {
                i++;
                continue;
            }
            if (c == '\'') state->in_char = false;
            continue;
        }

        if (c == '/' && cnext == '/') {
            in_line_comment = true;
            continue;
        }
        if (c == '/' && cnext == '*') {
            state->in_block_comment = true;
            i++;
            continue;
        }
        if (c == '\"') {
            state->in_string = true;
            continue;
        }
        if (c == '\'') {
            state->in_char = true;
            continue;
        }
        dst[j++] = c;
    }
    dst[j] = '\0';
}

static void check_file_custom(const LintConfig *cfg, const char *path, LintStats *stats) {
    FILE *fp = fopen(path, "r");
    if (!fp) {
        emit_diag("error", path, 0, 0, "fs", "failed to open file for custom checks");
        stats->errors++;
        return;
    }

    char raw[8192];
    char clean[8192];
    ScanState state = {false, false, false};
    bool in_function = false;
    int brace_depth = 0;
    int func_base_depth = 0;
    int func_start_line = 0;
    int func_lines = 0;
    int func_max_nesting = 0;
    bool sig_collecting = false;
    int sig_start_line = 0;
    char signature[8192] = {0};

    int lineno = 0;
    while (fgets(raw, sizeof(raw), fp)) {
        lineno++;
        strip_trailing_newline(raw);
        sanitize_for_parse(raw, clean, sizeof(clean), &state);

        size_t line_len = strlen(raw);
        if (cfg->max_line_length > 0 && line_len > (size_t)cfg->max_line_length) {
            emit_diag("warn", path, lineno, 1, "style", "long line");
            stats->warnings++;
        }

        if (line_has_direct_environment_access(clean)) {
            if (!is_getenv_allowed_in_file(cfg, path)) {
                emit_diag("error", path, lineno, 1, "api",
                          "direct environment access is only allowed in explicitly whitelisted files");
                stats->errors++;
            }
        }
        if (line_has_direct_console_output(clean) && !is_stdio_allowed_in_file(cfg, path)) {
            emit_diag("error", path, lineno, 1, "logging", "direct console output is forbidden; use tagged logging");
            stats->errors++;
        }
        if (line_has_platform_debug_output(clean) && !is_stdio_allowed_in_file(cfg, path)) {
            emit_diag("error", path, lineno, 1, "logging", "platform debug output is forbidden; use tagged logging");
            stats->errors++;
        }
        if ((line_has_call(clean, "system") || line_has_call(clean, "popen")) && !is_shell_allowed_in_file(cfg, path)) {
            emit_diag("error", path, lineno, 1, "process",
                      "shell execution is forbidden; launch an argv vector directly");
            stats->errors++;
        }

        if (!in_function && brace_depth == 0) {
            if (sig_collecting) {
                if (sig_start_line == 0) sig_start_line = lineno;
                if (signature[0] == '\0') {
                    snprintf(signature, sizeof(signature), "%s", clean);
                } else {
                    size_t used = strnlen(signature, sizeof(signature));
                    if (used + 1 < sizeof(signature)) {
                        size_t room = sizeof(signature) - used - 1;
                        size_t need = strlen(clean);
                        if (need + 1 > room) need = room - 1;
                        signature[used] = ' ';
                        signature[used + 1] = '\0';
                        strncat(signature, clean, need);
                    }
                }
                if (strchr(clean, ';')) {
                    signature[0] = '\0';
                    sig_collecting = false;
                }
            } else if (strchr(clean, '(') && !line_has_control_prefix(clean) && strncmp(clean, "#", 1) != 0) {
                signature[0] = '\0';
                snprintf(signature, sizeof(signature), "%s", clean);
                sig_start_line = lineno;
                sig_collecting = true;
            }

            char *brace = strchr(clean, '{');
            if (sig_collecting && brace) {
                *brace = '\0';
                if (looks_like_function_signature(signature)) {
                    in_function = true;
                    func_base_depth = brace_depth;
                    func_start_line = sig_start_line;
                    func_lines = 1;
                    func_max_nesting = 1;
                } else {
                    sig_collecting = false;
                    signature[0] = '\0';
                }
            }
        }

        if (in_function && lineno != func_start_line) func_lines++;

        for (size_t i = 0; i < strlen(clean); i++) {
            char c = clean[i];
            if (c == '{') {
                brace_depth++;
                if (in_function) {
                    int nesting = brace_depth - func_base_depth;
                    if (nesting > func_max_nesting) func_max_nesting = nesting;
                }
                continue;
            }
            if (c == '}') {
                if (brace_depth > 0) brace_depth--;
                if (in_function && brace_depth < func_base_depth + 1) {
                    if (cfg->max_function_lines > 0 && func_lines > cfg->max_function_lines) {
                        emit_diag("warn", path, func_start_line, 1, "complexity", "function exceeds max lines");
                        stats->warnings++;
                    }
                    if (cfg->max_nesting_depth > 0 && func_max_nesting > cfg->max_nesting_depth) {
                        emit_diag("warn", path, func_start_line, 1, "complexity", "function exceeds max nesting depth");
                        stats->warnings++;
                    }
                    in_function = false;
                    break;
                }
            }
        }
    }

    fclose(fp);
}

void hl_lint_policy_run(const LintConfig *cfg, const StringList *files, LintStats *stats) {
    if (!cfg->run_custom) return;
    for (size_t i = 0; i < files->count; i++) {
        check_file_custom(cfg, files->items[i], stats);
    }
}
