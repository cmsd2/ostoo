/*
 * env_demo — print command-line arguments and environment variables.
 * Used to verify that argv and envp flow correctly to userspace programs.
 */

#include <string.h>
#include "ostoo.h"

extern char **environ;

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
