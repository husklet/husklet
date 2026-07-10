require '_h2ph_pre.ph';

no warnings qw(redefine misc);

unless(defined(&_THREAD_MUTEX_INTERNAL_H)) {
    eval 'sub _THREAD_MUTEX_INTERNAL_H () {1;}' unless defined(&_THREAD_MUTEX_INTERNAL_H);
    if((defined(&__WORDSIZE) ? &__WORDSIZE : undef) == 64) {
    }
    if((defined(&__WORDSIZE) ? &__WORDSIZE : undef) != 64) {
    }
    if((defined(&__WORDSIZE) ? &__WORDSIZE : undef) == 64) {
	eval 'sub __PTHREAD_MUTEX_HAVE_PREV () {1;}' unless defined(&__PTHREAD_MUTEX_HAVE_PREV);
    } else {
	eval 'sub __PTHREAD_MUTEX_HAVE_PREV () {0;}' unless defined(&__PTHREAD_MUTEX_HAVE_PREV);
    }
    if((defined(&__PTHREAD_MUTEX_HAVE_PREV) ? &__PTHREAD_MUTEX_HAVE_PREV : undef) == 1) {
	eval 'sub __PTHREAD_MUTEX_INITIALIZER {
	    my($__kind) = @_;
    	    eval q(0, 0, 0, 0, $__kind, 0, { 0, 0});
	}' unless defined(&__PTHREAD_MUTEX_INITIALIZER);
    } else {
	eval 'sub __PTHREAD_MUTEX_INITIALIZER {
	    my($__kind) = @_;
    	    eval q(0, 0, 0, $__kind, 0, { 0});
	}' unless defined(&__PTHREAD_MUTEX_INITIALIZER);
    }
}
1;
