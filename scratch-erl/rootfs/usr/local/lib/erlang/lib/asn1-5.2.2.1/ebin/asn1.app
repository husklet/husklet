{application, asn1,
 [{description, "The Erlang ASN1 compiler version 5.2.2.1"},
  {vsn, "5.2.2.1"},
  {modules, [
        asn1rt_nif
             ]},
  {registered, [
	asn1_ns,
	asn1db
		]},
  {env, []},
  {applications, [kernel, stdlib]},
  {runtime_dependencies, ["stdlib-3.13","kernel-7.0","erts-11.0"]}
  ]}.
