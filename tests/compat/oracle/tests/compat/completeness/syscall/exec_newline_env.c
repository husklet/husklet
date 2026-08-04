#include "compat.h"
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
extern char **environ;
/* An env value containing a newline must cross execve intact -- not split into a second entry. */
int main(int argc, char **argv){
  if (argc>1 && strcmp(argv[1],"child")==0){
    const char *v=getenv("NLVAR");
    int value_ok=(v && strcmp(v,"a\nb")==0);
    int split_entry=0;
    for (char **e=environ; *e; e++) if (strcmp(*e,"b")==0) split_entry=1;
    printf("exec_newline value_ok=%d split_entry=%d\n", value_ok, split_entry);
    return 0;
  }
  char *env[]={ (char*)"NLVAR=a\nb", (char*)0 };
  char *av[]={ argv[0], (char*)"child", (char*)0 };
  execve(argv[0], av, env);
  perror("execve"); return 1;
}
