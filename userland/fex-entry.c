/*
 * pd2-fex: the entry point of App-provided FEX. The Launcher engine finds it through PD2_APP_FEX
 * and puts it at the head of every guest's start chain. It execs the App userland's FEX with the
 * same arguments and the same environment: it never sets, changes or removes a variable, so every
 * FEX_* value the Launcher sets reaches FEX as is.
 *
 * FEX_PATH is the userland FEX, compiled in by build-entry.sh ($USERLAND/usr/bin/FEX).
 */
#include <stdio.h>
#include <unistd.h>

#ifndef FEX_PATH
#error "FEX_PATH must name the userland FEX"
#endif

int main(int argc, char **argv) {
    (void)argc;
    execv(FEX_PATH, argv);
    perror("pd2-fex: " FEX_PATH);
    return 127;
}
