#include <iostream>
#include <numeric>
#include <vector>
int main(){ std::vector<long> v(1000); std::iota(v.begin(),v.end(),1); std::cout << "R=" << std::accumulate(v.begin(),v.end(),0L) << "\n"; }
