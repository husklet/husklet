require '_h2ph_pre.ph';

no warnings qw(redefine misc);

unless(defined(&_BITS_FLOATN_H)) {
    eval 'sub _BITS_FLOATN_H () {1;}' unless defined(&_BITS_FLOATN_H);
    require 'features.ph';
    require 'bits/long-double.ph';
    unless(defined(&__NO_LONG_DOUBLE_MATH)) {
	eval 'sub __HAVE_FLOAT128 () {1;}' unless defined(&__HAVE_FLOAT128);
    } else {
	eval 'sub __HAVE_FLOAT128 () {0;}' unless defined(&__HAVE_FLOAT128);
    }
    eval 'sub __HAVE_DISTINCT_FLOAT128 () {0;}' unless defined(&__HAVE_DISTINCT_FLOAT128);
    eval 'sub __HAVE_FLOAT64X () { &__HAVE_FLOAT128;}' unless defined(&__HAVE_FLOAT64X);
    eval 'sub __HAVE_FLOAT64X_LONG_DOUBLE () { &__HAVE_FLOAT128;}' unless defined(&__HAVE_FLOAT64X_LONG_DOUBLE);
    unless(defined(&__ASSEMBLER__)) {
	if((defined(&__HAVE_FLOAT128) ? &__HAVE_FLOAT128 : undef)) {
	    if(! &__GNUC_PREREQ (7, 0) || (defined (&__cplusplus)  && ! &__GNUC_PREREQ (13, 0))) {
		eval 'sub __f128 {
		    my($x) = @_;
    		    eval q($x &l);
		}' unless defined(&__f128);
	    } else {
		eval 'sub __f128 {
		    my($x) = @_;
    		    eval q($x &f128);
		}' unless defined(&__f128);
	    }
	}
	if((defined(&__HAVE_FLOAT128) ? &__HAVE_FLOAT128 : undef)) {
	    if(! &__GNUC_PREREQ (7, 0) || (defined (&__cplusplus)  && ! &__GNUC_PREREQ (13, 0))) {
		eval 'sub __CFLOAT128 () { &_Complex \'long double\';}' unless defined(&__CFLOAT128);
	    } else {
		eval 'sub __CFLOAT128 () { &_Complex  &_Float128;}' unless defined(&__CFLOAT128);
	    }
	}
	if((defined(&__HAVE_FLOAT128) ? &__HAVE_FLOAT128 : undef)) {
	    if(! &__GNUC_PREREQ (7, 0) || (defined (&__cplusplus)  && ! &__GNUC_PREREQ (13, 0))) {
	    }
	    if(! &__GNUC_PREREQ (7, 0)) {
		eval 'sub __builtin_huge_valf128 () {
		    eval q(( &__builtin_huge_vall ()));
		}' unless defined(&__builtin_huge_valf128);
		eval 'sub __builtin_inff128 () {
		    eval q(( &__builtin_infl ()));
		}' unless defined(&__builtin_inff128);
		eval 'sub __builtin_nanf128 {
		    my($x) = @_;
    		    eval q(( &__builtin_nanl ($x)));
		}' unless defined(&__builtin_nanf128);
		eval 'sub __builtin_nansf128 {
		    my($x) = @_;
    		    eval q(( &__builtin_nansl ($x)));
		}' unless defined(&__builtin_nansf128);
	    }
	}
    }
    require 'bits/floatn-common.ph';
}
1;
