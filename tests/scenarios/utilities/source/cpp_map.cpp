#include <cstdio>
#include <map>
#include <string>
int main(){std::map<std::string,int> m;const char* w[]={"a","b","a","c","b","a"};for(auto s:w)m[s]++;printf("MAP %d %d %d %zu\n",m["a"],m["b"],m["c"],m.size());return 0;}
