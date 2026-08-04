fn main(){let n=1000000usize;let mut c=vec![false;n];let mut k=0u64;let mut i=2usize;while i<n{if !c[i]{k+=1;let mut j=i*i;while j<n{c[j]=true;j+=i;}}i+=1;}println!("PRIMES {}",k);}
