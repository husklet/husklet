#!/usr/bin/perl
use warnings;
use strict;
use Debian::Debhelper::Dh_Lib;


deprecated_functionality('The autotools-dev sequence is deprecated and replaced by dh in debhelper (>= 9.20160115)', 12);

insert_before("dh_auto_configure", "dh_autotools-dev_updateconfig");
insert_before("dh_clean", "dh_autotools-dev_restoreconfig");

1;
