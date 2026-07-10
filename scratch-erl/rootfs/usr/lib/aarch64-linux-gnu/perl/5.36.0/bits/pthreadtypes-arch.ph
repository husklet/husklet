require '_h2ph_pre.ph';

no warnings qw(redefine misc);

unless(defined(&_BITS_PTHREADTYPES_ARCH_H)) {
    eval 'sub _BITS_PTHREADTYPES_ARCH_H () {1;}' unless defined(&_BITS_PTHREADTYPES_ARCH_H);
    require 'bits/endian.ph';
    if(defined(&__ILP32__)) {
	eval 'sub __SIZEOF_PTHREAD_ATTR_T () {32;}' unless defined(&__SIZEOF_PTHREAD_ATTR_T);
	eval 'sub __SIZEOF_PTHREAD_MUTEX_T () {32;}' unless defined(&__SIZEOF_PTHREAD_MUTEX_T);
	eval 'sub __SIZEOF_PTHREAD_MUTEXATTR_T () {4;}' unless defined(&__SIZEOF_PTHREAD_MUTEXATTR_T);
	eval 'sub __SIZEOF_PTHREAD_CONDATTR_T () {4;}' unless defined(&__SIZEOF_PTHREAD_CONDATTR_T);
	eval 'sub __SIZEOF_PTHREAD_RWLOCK_T () {48;}' unless defined(&__SIZEOF_PTHREAD_RWLOCK_T);
	eval 'sub __SIZEOF_PTHREAD_BARRIER_T () {20;}' unless defined(&__SIZEOF_PTHREAD_BARRIER_T);
	eval 'sub __SIZEOF_PTHREAD_BARRIERATTR_T () {4;}' unless defined(&__SIZEOF_PTHREAD_BARRIERATTR_T);
    } else {
	eval 'sub __SIZEOF_PTHREAD_ATTR_T () {64;}' unless defined(&__SIZEOF_PTHREAD_ATTR_T);
	eval 'sub __SIZEOF_PTHREAD_MUTEX_T () {48;}' unless defined(&__SIZEOF_PTHREAD_MUTEX_T);
	eval 'sub __SIZEOF_PTHREAD_MUTEXATTR_T () {8;}' unless defined(&__SIZEOF_PTHREAD_MUTEXATTR_T);
	eval 'sub __SIZEOF_PTHREAD_CONDATTR_T () {8;}' unless defined(&__SIZEOF_PTHREAD_CONDATTR_T);
	eval 'sub __SIZEOF_PTHREAD_RWLOCK_T () {56;}' unless defined(&__SIZEOF_PTHREAD_RWLOCK_T);
	eval 'sub __SIZEOF_PTHREAD_BARRIER_T () {32;}' unless defined(&__SIZEOF_PTHREAD_BARRIER_T);
	eval 'sub __SIZEOF_PTHREAD_BARRIERATTR_T () {8;}' unless defined(&__SIZEOF_PTHREAD_BARRIERATTR_T);
    }
    eval 'sub __SIZEOF_PTHREAD_COND_T () {48;}' unless defined(&__SIZEOF_PTHREAD_COND_T);
    eval 'sub __SIZEOF_PTHREAD_RWLOCKATTR_T () {8;}' unless defined(&__SIZEOF_PTHREAD_RWLOCKATTR_T);
    eval 'sub __LOCK_ALIGNMENT () {1;}' unless defined(&__LOCK_ALIGNMENT);
    eval 'sub __ONCE_ALIGNMENT () {1;}' unless defined(&__ONCE_ALIGNMENT);
}
1;
