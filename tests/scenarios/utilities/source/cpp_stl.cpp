#include <cstdio>
#include <vector>
#include <numeric>
#include <algorithm>
int main(){std::vector<long> v;for(long i=1;i<=1000000;i++)v.push_back(i);std::sort(v.begin(),v.end(),std::greater<long>());long s=std::accumulate(v.begin(),v.end(),0L);printf("SUM %ld %ld\n",s,v.front());return 0;}
