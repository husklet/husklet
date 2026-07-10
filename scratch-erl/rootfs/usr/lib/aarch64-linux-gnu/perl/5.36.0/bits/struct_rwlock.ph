require '_h2ph_pre.ph';

no warnings qw(redefine misc);

unless(defined(&_RWLOCK_INTERNAL_H)) {
    eval 'sub _RWLOCK_INTERNAL_H () {1;}' unless defined(&_RWLOCK_INTERNAL_H);
    eval 'sub __PTHREAD_RWLOCK_INITIALIZER {
        my($__flags) = @_;
	    eval q(0, 0, 0, 0, 0, 0, 0, 0, 0, 0, $__flags);
    }' unless defined(&__PTHREAD_RWLOCK_INITIALIZER);
}
1;
