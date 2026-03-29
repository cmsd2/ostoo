/*
 * Minimal userspace shell for ostoo.
 *
 * Reads raw keypresses from stdin (fd 0), performs its own line editing,
 * and dispatches built-in commands or spawns programs via posix_spawn.
 *
 * Built-in commands: echo, pwd, cd, ls, cat, pid, export, env, unset, exit
 * External programs: spawn by path (e.g. /hello)
 */

#include <unistd.h>
#include <string.h>
#include <stdlib.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <spawn.h>
#include <fcntl.h>

extern char **environ;

/* ── small helpers (no libc printf to avoid buffering issues) ───────── */

static void puts_fd(int fd, const char *s) {
    write(fd, s, strlen(s));
}

static void puts_stdout(const char *s) {
    puts_fd(1, s);
}

static void put_char(char c) {
    write(1, &c, 1);
}

/* itoa for small positive numbers */
static void put_num(unsigned long n) {
    char buf[20];
    int i = 0;
    if (n == 0) { put_char('0'); return; }
    while (n > 0) {
        buf[i++] = '0' + (n % 10);
        n /= 10;
    }
    while (--i >= 0) put_char(buf[i]);
}

/* ── line editing ───────────────────────────────────────────────────── */

#define MAX_LINE 256

static char line_buf[MAX_LINE];
static int line_len;
static int line_cursor;

static void line_clear(void) {
    line_len = 0;
    line_cursor = 0;
}

static void print_prompt(void) {
    char cwd[256];
    if (syscall(SYS_getcwd, cwd, sizeof(cwd)) > 0) {
        puts_stdout("ostoo:");
        puts_stdout(cwd);
        puts_stdout("> ");
    } else {
        puts_stdout("ostoo> ");
    }
}

/* Read one line with basic editing. Returns 1 if got a line, 0 on Ctrl+D. */
static int read_line(void) {
    unsigned char c;
    line_clear();

    for (;;) {
        ssize_t n = read(0, &c, 1);
        if (n <= 0) return 0; /* EOF / error */

        switch (c) {
        case '\n':
            put_char('\n');
            line_buf[line_len] = '\0';
            return 1;

        case 0x7F: /* DEL (backspace) */
            if (line_cursor > 0) {
                /* Move chars after cursor left */
                memmove(&line_buf[line_cursor - 1],
                        &line_buf[line_cursor],
                        line_len - line_cursor);
                line_cursor--;
                line_len--;
                /* Erase on terminal: backspace, rewrite tail, space to erase, back up */
                put_char('\b');
                write(1, &line_buf[line_cursor], line_len - line_cursor);
                put_char(' ');
                for (int i = 0; i <= line_len - line_cursor; i++)
                    put_char('\b');
            }
            break;

        case 0x03: /* Ctrl+C */
            puts_stdout("^C\n");
            line_clear();
            print_prompt();
            break;

        case 0x04: /* Ctrl+D */
            if (line_len == 0) return 0;
            break;

        default:
            if (c >= 0x20 && c < 0x7F && line_len < MAX_LINE - 1) {
                /* Insert character at cursor */
                if (line_cursor < line_len) {
                    memmove(&line_buf[line_cursor + 1],
                            &line_buf[line_cursor],
                            line_len - line_cursor);
                }
                line_buf[line_cursor] = c;
                line_cursor++;
                line_len++;
                /* Echo: write from cursor to end, then back up */
                write(1, &line_buf[line_cursor - 1], line_len - line_cursor + 1);
                for (int i = 0; i < line_len - line_cursor; i++)
                    put_char('\b');
            }
            break;
        }
    }
}

/* ── string utilities ───────────────────────────────────────────────── */

/* Skip leading whitespace, return pointer to first non-space. */
static char *skip_ws(char *s) {
    while (*s == ' ' || *s == '\t') s++;
    return s;
}

/* Return pointer to next word (after current word), or end of string. */
static char *next_word(char *s) {
    while (*s && *s != ' ' && *s != '\t') s++;
    return skip_ws(s);
}

/* ── environment variable table ─────────────────────────────────────── */

#define MAX_ENV     64
#define MAX_ENV_LEN 256

static char  env_table[MAX_ENV][MAX_ENV_LEN];
static int   env_count = 0;
static char *env_ptrs[MAX_ENV + 1];

static void rebuild_env_ptrs(void) {
    for (int i = 0; i < env_count; i++)
        env_ptrs[i] = env_table[i];
    env_ptrs[env_count] = (char *)0;
}

/* Set "KEY=VALUE" in the table. Returns 0 on success, -1 on error. */
static int env_set(const char *assignment) {
    const char *eq = assignment;
    while (*eq && *eq != '=') eq++;
    if (*eq != '=') return -1;

    int nlen = eq - assignment;
    int idx = -1;
    for (int i = 0; i < env_count; i++) {
        if (memcmp(env_table[i], assignment, nlen) == 0 && env_table[i][nlen] == '=') {
            idx = i;
            break;
        }
    }

    int len = strlen(assignment);
    if (len >= MAX_ENV_LEN) return -1;

    if (idx >= 0) {
        memcpy(env_table[idx], assignment, len + 1);
    } else {
        if (env_count >= MAX_ENV) return -1;
        memcpy(env_table[env_count], assignment, len + 1);
        env_count++;
    }
    rebuild_env_ptrs();
    return 0;
}

/* Remove a variable by name (just the name, no '='). */
static void env_unset(const char *name) {
    int nlen = strlen(name);
    int idx = -1;
    for (int i = 0; i < env_count; i++) {
        if (memcmp(env_table[i], name, nlen) == 0 && env_table[i][nlen] == '=') {
            idx = i;
            break;
        }
    }
    if (idx < 0) return;
    for (int i = idx; i < env_count - 1; i++)
        memcpy(env_table[i], env_table[i + 1], MAX_ENV_LEN);
    env_count--;
    rebuild_env_ptrs();
}

/* Import initial environment from the stack (set by kernel via envp). */
static void env_init(void) {
    if (!environ) return;
    for (int i = 0; environ[i] && env_count < MAX_ENV; i++) {
        int len = strlen(environ[i]);
        if (len < MAX_ENV_LEN) {
            memcpy(env_table[env_count], environ[i], len + 1);
            env_count++;
        }
    }
    rebuild_env_ptrs();
}

/* Get the value of an environment variable (returns pointer into env_table). */
static const char *env_get(const char *name) {
    int nlen = strlen(name);
    for (int i = 0; i < env_count; i++) {
        if (memcmp(env_table[i], name, nlen) == 0 && env_table[i][nlen] == '=')
            return env_table[i] + nlen + 1;
    }
    return (const char *)0;
}

/*
 * If `cmd` contains '/', use it as-is. Otherwise, search each PATH directory
 * for an executable. Writes the resolved path into `out` (capacity `outsz`).
 * Returns 1 on success, 0 if not found.
 */
static int resolve_command(const char *cmd, char *out, int outsz) {
    /* Absolute or relative path — use directly. */
    for (const char *p = cmd; *p; p++) {
        if (*p == '/') {
            int len = strlen(cmd);
            if (len >= outsz) return 0;
            memcpy(out, cmd, len + 1);
            return 1;
        }
    }

    const char *path = env_get("PATH");
    if (!path) return 0;

    /* Walk colon-separated PATH entries. */
    while (*path) {
        const char *sep = path;
        while (*sep && *sep != ':') sep++;
        int dlen = sep - path;
        int clen = strlen(cmd);

        if (dlen + 1 + clen < outsz) {
            memcpy(out, path, dlen);
            out[dlen] = '/';
            memcpy(out + dlen + 1, cmd, clen + 1);

            /* Probe: try to open the file. */
            int fd = open(out, O_RDONLY);
            if (fd >= 0) {
                close(fd);
                return 1;
            }
        }

        path = *sep ? sep + 1 : sep;
    }
    return 0;
}

/* ── built-in commands ──────────────────────────────────────────────── */

static void cmd_echo(char *args) {
    puts_stdout(args);
    put_char('\n');
}

static void cmd_pwd(void) {
    char buf[256];
    if (syscall(SYS_getcwd, buf, sizeof(buf)) > 0) {
        puts_stdout(buf);
        put_char('\n');
    } else {
        puts_stdout("getcwd failed\n");
    }
}

static void cmd_cd(char *path) {
    if (!*path) path = "/";
    if (syscall(SYS_chdir, path) < 0) {
        puts_stdout("cd: ");
        puts_stdout(path);
        puts_stdout(": not found\n");
    }
}

static void cmd_ls(char *path) {
    if (!*path) path = ".";

    /* We need to open the path as a directory and use getdents64. */
    int fd = open(path, O_RDONLY | O_DIRECTORY);
    if (fd < 0) {
        /* Try without O_DIRECTORY */
        fd = open(path, O_RDONLY);
        if (fd < 0) {
            puts_stdout("ls: ");
            puts_stdout(path);
            puts_stdout(": not found\n");
            return;
        }
    }

    /* linux_dirent64 layout: d_ino(8), d_off(8), d_reclen(2), d_type(1), d_name... */
    char dbuf[2048];
    for (;;) {
        long nread = syscall(SYS_getdents64, fd, dbuf, sizeof(dbuf));
        if (nread <= 0) break;

        long pos = 0;
        while (pos < nread) {
            unsigned short reclen = *(unsigned short *)(dbuf + pos + 16);
            unsigned char d_type = *(unsigned char *)(dbuf + pos + 18);
            char *name = dbuf + pos + 19;

            if (d_type == 4) {
                puts_stdout("  [DIR]  ");
            } else {
                puts_stdout("         ");
            }
            puts_stdout(name);
            put_char('\n');

            pos += reclen;
        }
    }

    close(fd);
}

static void cmd_cat(char *path) {
    if (!*path) {
        puts_stdout("usage: cat <file>\n");
        return;
    }

    int fd = open(path, O_RDONLY);
    if (fd < 0) {
        puts_stdout("cat: ");
        puts_stdout(path);
        puts_stdout(": not found\n");
        return;
    }

    char buf[512];
    for (;;) {
        ssize_t n = read(fd, buf, sizeof(buf));
        if (n <= 0) break;
        write(1, buf, n);
    }

    close(fd);
}

static void cmd_run(char *cmdline) {
    /* First word is the program name/path. */
    char *cmd = cmdline;
    char *end = cmd;
    while (*end && *end != ' ' && *end != '\t') end++;

    /* Null-terminate the command word. */
    int has_args = (*end != '\0');
    if (has_args) *end = '\0';

    /* Resolve command via PATH if it doesn't contain '/'. */
    char resolved[MAX_LINE];
    if (!resolve_command(cmd, resolved, sizeof(resolved))) {
        puts_stdout(cmd);
        puts_stdout(": not found\n");
        return;
    }

    /* Build argv: argv[0] = command name, then remaining words, NULL-terminated. */
    char *argv[16];
    int argc = 0;
    argv[argc++] = cmd;

    if (has_args) {
        char *rest = skip_ws(end + 1);
        while (*rest && argc < 15) {
            argv[argc++] = rest;
            char *w = rest;
            while (*w && *w != ' ' && *w != '\t') w++;
            if (*w) { *w = '\0'; rest = skip_ws(w + 1); }
            else break;
        }
    }
    argv[argc] = (char *)0;

    /* Use posix_spawn (musl uses clone(CLONE_VM|CLONE_VFORK) + execve). */
    pid_t child_pid;
    int err = posix_spawn(&child_pid, resolved, 0, 0, argv, env_ptrs);
    if (err != 0) {
        puts_stdout(cmd);
        puts_stdout(": failed to spawn\n");
        return;
    }

    /* Wait for child to finish. */
    int status = 0;
    waitpid(child_pid, &status, 0);
}

/* ── main loop ──────────────────────────────────────────────────────── */

int main(void) {
    env_init();
    puts_stdout("\nostoo userspace shell\n");

    for (;;) {
        print_prompt();
        if (!read_line()) {
            puts_stdout("\nexit\n");
            break;
        }

        char *cmd = skip_ws(line_buf);
        if (!*cmd) continue;

        char *args = next_word(cmd);
        /* Null-terminate the command word for easy comparison. */
        {
            char *p = cmd;
            while (*p && *p != ' ' && *p != '\t') p++;
            if (*p) { *p = '\0'; args = skip_ws(p + 1); }
        }

        if (strcmp(cmd, "echo") == 0) {
            cmd_echo(args);
        } else if (strcmp(cmd, "pwd") == 0) {
            cmd_pwd();
        } else if (strcmp(cmd, "cd") == 0) {
            cmd_cd(args);
        } else if (strcmp(cmd, "ls") == 0) {
            cmd_ls(args);
        } else if (strcmp(cmd, "cat") == 0) {
            cmd_cat(args);
        } else if (strcmp(cmd, "pid") == 0) {
            put_num(getpid());
            put_char('\n');
        } else if (strcmp(cmd, "export") == 0) {
            if (!*args) {
                for (int i = 0; i < env_count; i++) {
                    puts_stdout("export ");
                    puts_stdout(env_table[i]);
                    put_char('\n');
                }
            } else {
                if (env_set(args) < 0)
                    puts_stdout("export: invalid or table full\n");
            }
        } else if (strcmp(cmd, "env") == 0) {
            for (int i = 0; i < env_count; i++) {
                puts_stdout(env_table[i]);
                put_char('\n');
            }
        } else if (strcmp(cmd, "unset") == 0) {
            if (*args)
                env_unset(args);
            else
                puts_stdout("usage: unset VAR\n");
        } else if (strcmp(cmd, "exit") == 0) {
            break;
        } else if (strcmp(cmd, "help") == 0) {
            puts_stdout("Commands: echo, pwd, cd, ls, cat, pid, export, env, unset, exit, help\n");
            puts_stdout("Or run a program by name (e.g. env_demo) or path (e.g. /bin/env_demo)\n");
        } else {
            /* Reconstruct full cmdline for spawning (cmd was null-terminated). */
            char fullcmd[MAX_LINE];
            int clen = strlen(cmd);
            memcpy(fullcmd, cmd, clen);
            if (*args) {
                fullcmd[clen] = ' ';
                strcpy(fullcmd + clen + 1, args);
            } else {
                fullcmd[clen] = '\0';
            }
            cmd_run(fullcmd);
        }
    }

    _exit(0);
    return 0;
}
