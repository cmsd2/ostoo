/*
 * env_demo — print command-line arguments and environment variables.
 * Used to verify that argv and envp flow correctly to userspace programs.
 */

#include <unistd.h>
#include <string.h>

extern char **environ;

static void puts_stdout(const char *s) {
    write(1, s, strlen(s));
}

static void put_num(int n) {
    char buf[12];
    int i = 0;
    if (n == 0) { write(1, "0", 1); return; }
    if (n < 0) { write(1, "-", 1); n = -n; }
    while (n > 0) { buf[i++] = '0' + (n % 10); n /= 10; }
    while (--i >= 0) write(1, &buf[i], 1);
}

int main(int argc, char *argv[]) {
    puts_stdout("argc=");
    put_num(argc);
    write(1, "\n", 1);

    for (int i = 0; i < argc; i++) {
        puts_stdout("argv[");
        put_num(i);
        puts_stdout("]=");
        puts_stdout(argv[i]);
        write(1, "\n", 1);
    }

    write(1, "\nEnvironment:\n", 14);
    if (environ) {
        for (int i = 0; environ[i]; i++) {
            puts_stdout("  ");
            puts_stdout(environ[i]);
            write(1, "\n", 1);
        }
    } else {
        puts_stdout("  (none)\n");
    }

    return 0;
}
