/*
 * Minimal userspace shell for ostoo.
 *
 * Reads raw keypresses from stdin (fd 0), performs its own line editing,
 * and dispatches built-in commands or spawns programs via posix_spawn.
 *
 * Built-in commands: echo, pwd, cd, ls, cat, exit, pid
 * External programs: spawn by path (e.g. /hello)
 */

#include <unistd.h>
#include <string.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <spawn.h>
#include <fcntl.h>

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
    /* First word is the program path. */
    char *path = cmdline;
    char *end = path;
    while (*end && *end != ' ' && *end != '\t') end++;

    /* Null-terminate the path. */
    int has_args = (*end != '\0');
    if (has_args) *end = '\0';

    /* Build argv: argv[0] = path, then remaining words, NULL-terminated. */
    char *argv[16];
    int argc = 0;
    argv[argc++] = path;

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
    int err = posix_spawn(&child_pid, path, 0, 0, argv, (char **)0);
    if (err != 0) {
        puts_stdout(path);
        puts_stdout(": not found\n");
        return;
    }

    /* Wait for child to finish. */
    int status = 0;
    waitpid(child_pid, &status, 0);
}

/* ── main loop ──────────────────────────────────────────────────────── */

int main(void) {
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
        } else if (strcmp(cmd, "exit") == 0) {
            break;
        } else if (strcmp(cmd, "help") == 0) {
            puts_stdout("Commands: echo, pwd, cd, ls, cat, pid, exit, help\n");
            puts_stdout("Or run a program by path (e.g. /hello)\n");
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
