require '_h2ph_pre.ph';

no warnings qw(redefine misc);

unless(defined(&__ASM_SVE_CONTEXT_H)) {
    eval 'sub __ASM_SVE_CONTEXT_H () {1;}' unless defined(&__ASM_SVE_CONTEXT_H);
    require 'linux/types.ph';
    eval 'sub __SVE_VQ_BYTES () {16;}' unless defined(&__SVE_VQ_BYTES);
    eval 'sub __SVE_VQ_MIN () {1;}' unless defined(&__SVE_VQ_MIN);
    eval 'sub __SVE_VQ_MAX () {512;}' unless defined(&__SVE_VQ_MAX);
    eval 'sub __SVE_VL_MIN () {( &__SVE_VQ_MIN *  &__SVE_VQ_BYTES);}' unless defined(&__SVE_VL_MIN);
    eval 'sub __SVE_VL_MAX () {( &__SVE_VQ_MAX *  &__SVE_VQ_BYTES);}' unless defined(&__SVE_VL_MAX);
    eval 'sub __SVE_NUM_ZREGS () {32;}' unless defined(&__SVE_NUM_ZREGS);
    eval 'sub __SVE_NUM_PREGS () {16;}' unless defined(&__SVE_NUM_PREGS);
    eval 'sub __sve_vl_valid {
        my($vl) = @_;
	    eval q((($vl) %  &__SVE_VQ_BYTES == 0 && ($vl) >=  &__SVE_VL_MIN  && ($vl) <=  &__SVE_VL_MAX));
    }' unless defined(&__sve_vl_valid);
    eval 'sub __sve_vq_from_vl {
        my($vl) = @_;
	    eval q((($vl) /  &__SVE_VQ_BYTES));
    }' unless defined(&__sve_vq_from_vl);
    eval 'sub __sve_vl_from_vq {
        my($vq) = @_;
	    eval q((($vq) *  &__SVE_VQ_BYTES));
    }' unless defined(&__sve_vl_from_vq);
    eval 'sub __SVE_ZREG_SIZE {
        my($vq) = @_;
	    eval q((( &__u32)($vq) *  &__SVE_VQ_BYTES));
    }' unless defined(&__SVE_ZREG_SIZE);
    eval 'sub __SVE_PREG_SIZE {
        my($vq) = @_;
	    eval q((( &__u32)($vq) * ( &__SVE_VQ_BYTES / 8)));
    }' unless defined(&__SVE_PREG_SIZE);
    eval 'sub __SVE_FFR_SIZE {
        my($vq) = @_;
	    eval q( &__SVE_PREG_SIZE($vq));
    }' unless defined(&__SVE_FFR_SIZE);
    eval 'sub __SVE_ZREGS_OFFSET () {0;}' unless defined(&__SVE_ZREGS_OFFSET);
    eval 'sub __SVE_ZREG_OFFSET {
        my($vq, $n) = @_;
	    eval q(( &__SVE_ZREGS_OFFSET +  &__SVE_ZREG_SIZE($vq) * ($n)));
    }' unless defined(&__SVE_ZREG_OFFSET);
    eval 'sub __SVE_ZREGS_SIZE {
        my($vq) = @_;
	    eval q(( &__SVE_ZREG_OFFSET($vq,  &__SVE_NUM_ZREGS) -  &__SVE_ZREGS_OFFSET));
    }' unless defined(&__SVE_ZREGS_SIZE);
    eval 'sub __SVE_PREGS_OFFSET {
        my($vq) = @_;
	    eval q(( &__SVE_ZREGS_OFFSET +  &__SVE_ZREGS_SIZE($vq)));
    }' unless defined(&__SVE_PREGS_OFFSET);
    eval 'sub __SVE_PREG_OFFSET {
        my($vq, $n) = @_;
	    eval q(( &__SVE_PREGS_OFFSET($vq) +  &__SVE_PREG_SIZE($vq) * ($n)));
    }' unless defined(&__SVE_PREG_OFFSET);
    eval 'sub __SVE_PREGS_SIZE {
        my($vq) = @_;
	    eval q(( &__SVE_PREG_OFFSET($vq,  &__SVE_NUM_PREGS) -  &__SVE_PREGS_OFFSET($vq)));
    }' unless defined(&__SVE_PREGS_SIZE);
    eval 'sub __SVE_FFR_OFFSET {
        my($vq) = @_;
	    eval q(( &__SVE_PREGS_OFFSET($vq) +  &__SVE_PREGS_SIZE($vq)));
    }' unless defined(&__SVE_FFR_OFFSET);
}
1;
