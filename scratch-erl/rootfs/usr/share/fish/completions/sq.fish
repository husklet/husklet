complete -c sq -n "__fish_use_subcommand" -l output-format -d 'Produces output in FORMAT, if possible' -r -f -a "{human-readable	,json	}"
complete -c sq -n "__fish_use_subcommand" -l output-version -d 'Produces output variant VERSION' -r
complete -c sq -n "__fish_use_subcommand" -l known-notation -d 'Adds NOTATION to the list of known notations' -r
complete -c sq -n "__fish_use_subcommand" -s h -l help -d 'Print help information'
complete -c sq -n "__fish_use_subcommand" -s V -l version -d 'Print version information'
complete -c sq -n "__fish_use_subcommand" -s f -l force -d 'Overwrites existing files'
complete -c sq -n "__fish_use_subcommand" -f -a "armor" -d 'Converts binary to ASCII'
complete -c sq -n "__fish_use_subcommand" -f -a "dearmor" -d 'Converts ASCII to binary'
complete -c sq -n "__fish_use_subcommand" -f -a "sign" -d 'Signs messages or data files'
complete -c sq -n "__fish_use_subcommand" -f -a "verify" -d 'Verifies signed messages or detached signatures'
complete -c sq -n "__fish_use_subcommand" -f -a "wkd" -d 'Interacts with Web Key Directories'
complete -c sq -n "__fish_use_subcommand" -f -a "keyserver" -d 'Interacts with keyservers'
complete -c sq -n "__fish_use_subcommand" -f -a "revoke" -d 'Generates revocation certificates'
complete -c sq -n "__fish_use_subcommand" -f -a "packet" -d 'Low-level packet manipulation'
complete -c sq -n "__fish_use_subcommand" -f -a "certify" -d 'Certifies a User ID for a Certificate'
complete -c sq -n "__fish_use_subcommand" -f -a "keyring" -d 'Manages collections of keys or certs'
complete -c sq -n "__fish_use_subcommand" -f -a "key" -d 'Manages keys'
complete -c sq -n "__fish_use_subcommand" -f -a "inspect" -d 'Inspects data, like file(1)'
complete -c sq -n "__fish_use_subcommand" -f -a "encrypt" -d 'Encrypts a message'
complete -c sq -n "__fish_use_subcommand" -f -a "decrypt" -d 'Decrypts a message'
complete -c sq -n "__fish_use_subcommand" -f -a "autocrypt" -d 'Communicates certificates using Autocrypt'
complete -c sq -n "__fish_use_subcommand" -f -a "help" -d 'Print this message or the help of the given subcommand(s)'
complete -c sq -n "__fish_seen_subcommand_from armor" -s o -l output -d 'Writes to FILE or stdout if omitted' -r
complete -c sq -n "__fish_seen_subcommand_from armor" -l label -d 'Selects the kind of armor header' -r -f -a "{auto	,message	,cert	,key	,sig	,file	}"
complete -c sq -n "__fish_seen_subcommand_from armor" -s h -l help -d 'Print help information'
complete -c sq -n "__fish_seen_subcommand_from dearmor" -s o -l output -d 'Writes to FILE or stdout if omitted' -r
complete -c sq -n "__fish_seen_subcommand_from dearmor" -s h -l help -d 'Print help information'
complete -c sq -n "__fish_seen_subcommand_from sign" -s o -l output -d 'Writes to FILE or stdout if omitted' -r
complete -c sq -n "__fish_seen_subcommand_from sign" -l private-key-store -d 'Provides parameters for private key store' -r
complete -c sq -n "__fish_seen_subcommand_from sign" -l merge -d 'Merges signatures from the input and SIGNED-MESSAGE' -r
complete -c sq -n "__fish_seen_subcommand_from sign" -l signer-key -d 'Signs using KEY' -r
complete -c sq -n "__fish_seen_subcommand_from sign" -s t -l time -d 'Chooses keys valid at the specified time and sets the signature\'s creation time' -r
complete -c sq -n "__fish_seen_subcommand_from sign" -l notation -d 'Adds a notation to the certification.' -r
complete -c sq -n "__fish_seen_subcommand_from sign" -s B -l binary -d 'Emits binary data'
complete -c sq -n "__fish_seen_subcommand_from sign" -l detached -d 'Creates a detached signature'
complete -c sq -n "__fish_seen_subcommand_from sign" -l cleartext-signature -d 'Creates a cleartext signature'
complete -c sq -n "__fish_seen_subcommand_from sign" -s a -l append -d 'Appends a signature to existing signature'
complete -c sq -n "__fish_seen_subcommand_from sign" -s n -l notarize -d 'Signs a message and all existing signatures'
complete -c sq -n "__fish_seen_subcommand_from sign" -s h -l help -d 'Print help information'
complete -c sq -n "__fish_seen_subcommand_from verify" -s o -l output -d 'Writes to FILE or stdout if omitted' -r
complete -c sq -n "__fish_seen_subcommand_from verify" -l detached -d 'Verifies a detached signature' -r
complete -c sq -n "__fish_seen_subcommand_from verify" -s n -l signatures -d 'Sets the threshold of valid signatures to N' -r
complete -c sq -n "__fish_seen_subcommand_from verify" -l signer-cert -d 'Verifies signatures with CERT' -r
complete -c sq -n "__fish_seen_subcommand_from verify" -s h -l help -d 'Print help information'
complete -c sq -n "__fish_seen_subcommand_from wkd; and not __fish_seen_subcommand_from url; and not __fish_seen_subcommand_from direct-url; and not __fish_seen_subcommand_from get; and not __fish_seen_subcommand_from generate; and not __fish_seen_subcommand_from help" -s n -l network-policy -d 'Sets the network policy to use' -r -f -a "{offline	,anonymized	,encrypted	,insecure	}"
complete -c sq -n "__fish_seen_subcommand_from wkd; and not __fish_seen_subcommand_from url; and not __fish_seen_subcommand_from direct-url; and not __fish_seen_subcommand_from get; and not __fish_seen_subcommand_from generate; and not __fish_seen_subcommand_from help" -s h -l help -d 'Print help information'
complete -c sq -n "__fish_seen_subcommand_from wkd; and not __fish_seen_subcommand_from url; and not __fish_seen_subcommand_from direct-url; and not __fish_seen_subcommand_from get; and not __fish_seen_subcommand_from generate; and not __fish_seen_subcommand_from help" -f -a "url" -d 'Prints the advanced Web Key Directory URL of an email address.'
complete -c sq -n "__fish_seen_subcommand_from wkd; and not __fish_seen_subcommand_from url; and not __fish_seen_subcommand_from direct-url; and not __fish_seen_subcommand_from get; and not __fish_seen_subcommand_from generate; and not __fish_seen_subcommand_from help" -f -a "direct-url" -d 'Prints the direct Web Key Directory URL of an email address.'
complete -c sq -n "__fish_seen_subcommand_from wkd; and not __fish_seen_subcommand_from url; and not __fish_seen_subcommand_from direct-url; and not __fish_seen_subcommand_from get; and not __fish_seen_subcommand_from generate; and not __fish_seen_subcommand_from help" -f -a "get" -d 'Queries for certs using Web Key Directory'
complete -c sq -n "__fish_seen_subcommand_from wkd; and not __fish_seen_subcommand_from url; and not __fish_seen_subcommand_from direct-url; and not __fish_seen_subcommand_from get; and not __fish_seen_subcommand_from generate; and not __fish_seen_subcommand_from help" -f -a "generate" -d 'Generates a Web Key Directory for the given domain and keys.'
complete -c sq -n "__fish_seen_subcommand_from wkd; and not __fish_seen_subcommand_from url; and not __fish_seen_subcommand_from direct-url; and not __fish_seen_subcommand_from get; and not __fish_seen_subcommand_from generate; and not __fish_seen_subcommand_from help" -f -a "help" -d 'Print this message or the help of the given subcommand(s)'
complete -c sq -n "__fish_seen_subcommand_from wkd; and __fish_seen_subcommand_from url" -s h -l help -d 'Print help information'
complete -c sq -n "__fish_seen_subcommand_from wkd; and __fish_seen_subcommand_from direct-url" -s h -l help -d 'Print help information'
complete -c sq -n "__fish_seen_subcommand_from wkd; and __fish_seen_subcommand_from get" -s o -l output -d 'Writes to FILE or stdout if omitted' -r
complete -c sq -n "__fish_seen_subcommand_from wkd; and __fish_seen_subcommand_from get" -s B -l binary -d 'Emits binary data'
complete -c sq -n "__fish_seen_subcommand_from wkd; and __fish_seen_subcommand_from get" -s h -l help -d 'Print help information'
complete -c sq -n "__fish_seen_subcommand_from wkd; and __fish_seen_subcommand_from generate" -s d -l direct-method -d 'Uses the direct method [default: advanced method]'
complete -c sq -n "__fish_seen_subcommand_from wkd; and __fish_seen_subcommand_from generate" -s s -l skip -d 'Skips certificates that do not have User IDs for given domain.'
complete -c sq -n "__fish_seen_subcommand_from wkd; and __fish_seen_subcommand_from generate" -s h -l help -d 'Print help information'
complete -c sq -n "__fish_seen_subcommand_from keyserver; and not __fish_seen_subcommand_from get; and not __fish_seen_subcommand_from send; and not __fish_seen_subcommand_from help" -s p -l policy -d 'Sets the network policy to use' -r -f -a "{offline	,anonymized	,encrypted	,insecure	}"
complete -c sq -n "__fish_seen_subcommand_from keyserver; and not __fish_seen_subcommand_from get; and not __fish_seen_subcommand_from send; and not __fish_seen_subcommand_from help" -s s -l server -d 'Sets the keyserver to use' -r
complete -c sq -n "__fish_seen_subcommand_from keyserver; and not __fish_seen_subcommand_from get; and not __fish_seen_subcommand_from send; and not __fish_seen_subcommand_from help" -s h -l help -d 'Print help information'
complete -c sq -n "__fish_seen_subcommand_from keyserver; and not __fish_seen_subcommand_from get; and not __fish_seen_subcommand_from send; and not __fish_seen_subcommand_from help" -f -a "get" -d 'Retrieves a key'
complete -c sq -n "__fish_seen_subcommand_from keyserver; and not __fish_seen_subcommand_from get; and not __fish_seen_subcommand_from send; and not __fish_seen_subcommand_from help" -f -a "send" -d 'Sends a key'
complete -c sq -n "__fish_seen_subcommand_from keyserver; and not __fish_seen_subcommand_from get; and not __fish_seen_subcommand_from send; and not __fish_seen_subcommand_from help" -f -a "help" -d 'Print this message or the help of the given subcommand(s)'
complete -c sq -n "__fish_seen_subcommand_from keyserver; and __fish_seen_subcommand_from get" -s o -l output -d 'Writes to FILE or stdout if omitted' -r
complete -c sq -n "__fish_seen_subcommand_from keyserver; and __fish_seen_subcommand_from get" -s B -l binary -d 'Emits binary data'
complete -c sq -n "__fish_seen_subcommand_from keyserver; and __fish_seen_subcommand_from get" -s h -l help -d 'Print help information'
complete -c sq -n "__fish_seen_subcommand_from keyserver; and __fish_seen_subcommand_from send" -s h -l help -d 'Print help information'
complete -c sq -n "__fish_seen_subcommand_from revoke; and not __fish_seen_subcommand_from certificate; and not __fish_seen_subcommand_from subkey; and not __fish_seen_subcommand_from userid; and not __fish_seen_subcommand_from help" -s h -l help -d 'Print help information'
complete -c sq -n "__fish_seen_subcommand_from revoke; and not __fish_seen_subcommand_from certificate; and not __fish_seen_subcommand_from subkey; and not __fish_seen_subcommand_from userid; and not __fish_seen_subcommand_from help" -f -a "certificate" -d 'Revoke a certificate'
complete -c sq -n "__fish_seen_subcommand_from revoke; and not __fish_seen_subcommand_from certificate; and not __fish_seen_subcommand_from subkey; and not __fish_seen_subcommand_from userid; and not __fish_seen_subcommand_from help" -f -a "subkey" -d 'Revoke a subkey'
complete -c sq -n "__fish_seen_subcommand_from revoke; and not __fish_seen_subcommand_from certificate; and not __fish_seen_subcommand_from subkey; and not __fish_seen_subcommand_from userid; and not __fish_seen_subcommand_from help" -f -a "userid" -d 'Revoke a User ID'
complete -c sq -n "__fish_seen_subcommand_from revoke; and not __fish_seen_subcommand_from certificate; and not __fish_seen_subcommand_from subkey; and not __fish_seen_subcommand_from userid; and not __fish_seen_subcommand_from help" -f -a "help" -d 'Print this message or the help of the given subcommand(s)'
complete -c sq -n "__fish_seen_subcommand_from revoke; and __fish_seen_subcommand_from certificate" -l certificate -d 'The certificate to revoke' -r
complete -c sq -n "__fish_seen_subcommand_from revoke; and __fish_seen_subcommand_from certificate" -l revocation-key -d 'Signs the revocation certificate using KEY' -r
complete -c sq -n "__fish_seen_subcommand_from revoke; and __fish_seen_subcommand_from certificate" -l private-key-store -d 'Provides parameters for private key store' -r
complete -c sq -n "__fish_seen_subcommand_from revoke; and __fish_seen_subcommand_from certificate" -s t -l time -d 'Chooses keys valid at the specified time and sets the revocation certificate\'s creation time' -r
complete -c sq -n "__fish_seen_subcommand_from revoke; and __fish_seen_subcommand_from certificate" -l notation -d 'Adds a notation to the certification.' -r
complete -c sq -n "__fish_seen_subcommand_from revoke; and __fish_seen_subcommand_from certificate" -s B -l binary -d 'Emits binary data'
complete -c sq -n "__fish_seen_subcommand_from revoke; and __fish_seen_subcommand_from certificate" -s h -l help -d 'Print help information'
complete -c sq -n "__fish_seen_subcommand_from revoke; and __fish_seen_subcommand_from subkey" -l certificate -d 'The certificate containing the subkey to revoke' -r
complete -c sq -n "__fish_seen_subcommand_from revoke; and __fish_seen_subcommand_from subkey" -l revocation-key -d 'Signs the revocation certificate using KEY' -r
complete -c sq -n "__fish_seen_subcommand_from revoke; and __fish_seen_subcommand_from subkey" -l private-key-store -d 'Provides parameters for private key store' -r
complete -c sq -n "__fish_seen_subcommand_from revoke; and __fish_seen_subcommand_from subkey" -s t -l time -d 'Chooses keys valid at the specified time and sets the revocation certificate\'s creation time' -r
complete -c sq -n "__fish_seen_subcommand_from revoke; and __fish_seen_subcommand_from subkey" -l notation -d 'Adds a notation to the certification.' -r
complete -c sq -n "__fish_seen_subcommand_from revoke; and __fish_seen_subcommand_from subkey" -s B -l binary -d 'Emits binary data'
complete -c sq -n "__fish_seen_subcommand_from revoke; and __fish_seen_subcommand_from subkey" -s h -l help -d 'Print help information'
complete -c sq -n "__fish_seen_subcommand_from revoke; and __fish_seen_subcommand_from userid" -l certificate -d 'The certificate containing the User ID to revoke' -r
complete -c sq -n "__fish_seen_subcommand_from revoke; and __fish_seen_subcommand_from userid" -l revocation-key -d 'Signs the revocation certificate using KEY' -r
complete -c sq -n "__fish_seen_subcommand_from revoke; and __fish_seen_subcommand_from userid" -l private-key-store -d 'Provides parameters for private key store' -r
complete -c sq -n "__fish_seen_subcommand_from revoke; and __fish_seen_subcommand_from userid" -s t -l time -d 'Chooses keys valid at the specified time and sets the revocation certificate\'s creation time' -r
complete -c sq -n "__fish_seen_subcommand_from revoke; and __fish_seen_subcommand_from userid" -l notation -d 'Adds a notation to the certification.' -r
complete -c sq -n "__fish_seen_subcommand_from revoke; and __fish_seen_subcommand_from userid" -s B -l binary -d 'Emits binary data'
complete -c sq -n "__fish_seen_subcommand_from revoke; and __fish_seen_subcommand_from userid" -s h -l help -d 'Print help information'
complete -c sq -n "__fish_seen_subcommand_from packet; and not __fish_seen_subcommand_from dump; and not __fish_seen_subcommand_from decrypt; and not __fish_seen_subcommand_from split; and not __fish_seen_subcommand_from join; and not __fish_seen_subcommand_from help" -s h -l help -d 'Print help information'
complete -c sq -n "__fish_seen_subcommand_from packet; and not __fish_seen_subcommand_from dump; and not __fish_seen_subcommand_from decrypt; and not __fish_seen_subcommand_from split; and not __fish_seen_subcommand_from join; and not __fish_seen_subcommand_from help" -f -a "dump" -d 'Lists packets'
complete -c sq -n "__fish_seen_subcommand_from packet; and not __fish_seen_subcommand_from dump; and not __fish_seen_subcommand_from decrypt; and not __fish_seen_subcommand_from split; and not __fish_seen_subcommand_from join; and not __fish_seen_subcommand_from help" -f -a "decrypt" -d 'Unwraps an encryption container'
complete -c sq -n "__fish_seen_subcommand_from packet; and not __fish_seen_subcommand_from dump; and not __fish_seen_subcommand_from decrypt; and not __fish_seen_subcommand_from split; and not __fish_seen_subcommand_from join; and not __fish_seen_subcommand_from help" -f -a "split" -d 'Splits a message into packets'
complete -c sq -n "__fish_seen_subcommand_from packet; and not __fish_seen_subcommand_from dump; and not __fish_seen_subcommand_from decrypt; and not __fish_seen_subcommand_from split; and not __fish_seen_subcommand_from join; and not __fish_seen_subcommand_from help" -f -a "join" -d 'Joins packets split across files'
complete -c sq -n "__fish_seen_subcommand_from packet; and not __fish_seen_subcommand_from dump; and not __fish_seen_subcommand_from decrypt; and not __fish_seen_subcommand_from split; and not __fish_seen_subcommand_from join; and not __fish_seen_subcommand_from help" -f -a "help" -d 'Print this message or the help of the given subcommand(s)'
complete -c sq -n "__fish_seen_subcommand_from packet; and __fish_seen_subcommand_from dump" -s o -l output -d 'Writes to FILE or stdout if omitted' -r
complete -c sq -n "__fish_seen_subcommand_from packet; and __fish_seen_subcommand_from dump" -l session-key -d 'Decrypts an encrypted message using SESSION-KEY' -r
complete -c sq -n "__fish_seen_subcommand_from packet; and __fish_seen_subcommand_from dump" -l mpis -d 'Prints cryptographic artifacts'
complete -c sq -n "__fish_seen_subcommand_from packet; and __fish_seen_subcommand_from dump" -s x -l hex -d 'Prints a hexdump'
complete -c sq -n "__fish_seen_subcommand_from packet; and __fish_seen_subcommand_from dump" -s h -l help -d 'Print help information'
complete -c sq -n "__fish_seen_subcommand_from packet; and __fish_seen_subcommand_from decrypt" -s o -l output -d 'Writes to FILE or stdout if omitted' -r
complete -c sq -n "__fish_seen_subcommand_from packet; and __fish_seen_subcommand_from decrypt" -l recipient-key -d 'Decrypts the message with KEY' -r
complete -c sq -n "__fish_seen_subcommand_from packet; and __fish_seen_subcommand_from decrypt" -l private-key-store -d 'Provides parameters for private key store' -r
complete -c sq -n "__fish_seen_subcommand_from packet; and __fish_seen_subcommand_from decrypt" -l session-key -d 'Decrypts an encrypted message using SESSION-KEY' -r
complete -c sq -n "__fish_seen_subcommand_from packet; and __fish_seen_subcommand_from decrypt" -s B -l binary -d 'Emits binary data'
complete -c sq -n "__fish_seen_subcommand_from packet; and __fish_seen_subcommand_from decrypt" -l dump-session-key -d 'Prints the session key to stderr'
complete -c sq -n "__fish_seen_subcommand_from packet; and __fish_seen_subcommand_from decrypt" -s h -l help -d 'Print help information'
complete -c sq -n "__fish_seen_subcommand_from packet; and __fish_seen_subcommand_from split" -s p -l prefix -d 'Writes to files with PREFIX [defaults: "FILE-" if FILE is set, or "output-" if read from stdin]' -r
complete -c sq -n "__fish_seen_subcommand_from packet; and __fish_seen_subcommand_from split" -s h -l help -d 'Print help information'
complete -c sq -n "__fish_seen_subcommand_from packet; and __fish_seen_subcommand_from join" -s o -l output -d 'Writes to FILE or stdout if omitted' -r
complete -c sq -n "__fish_seen_subcommand_from packet; and __fish_seen_subcommand_from join" -l label -d 'Selects the kind of armor header' -r -f -a "{auto	,message	,cert	,key	,sig	,file	}"
complete -c sq -n "__fish_seen_subcommand_from packet; and __fish_seen_subcommand_from join" -s B -l binary -d 'Emits binary data'
complete -c sq -n "__fish_seen_subcommand_from packet; and __fish_seen_subcommand_from join" -s h -l help -d 'Print help information'
complete -c sq -n "__fish_seen_subcommand_from certify" -s o -l output -d 'Writes to FILE or stdout if omitted' -r
complete -c sq -n "__fish_seen_subcommand_from certify" -l time -d 'Sets the certification time to TIME (as ISO 8601)' -r
complete -c sq -n "__fish_seen_subcommand_from certify" -s d -l depth -d 'Sets the trust depth' -r
complete -c sq -n "__fish_seen_subcommand_from certify" -s a -l amount -d 'Sets the amount of trust' -r
complete -c sq -n "__fish_seen_subcommand_from certify" -s r -l regex -d 'Adds a regular expression to constrain what a trusted introducer can certify' -r
complete -c sq -n "__fish_seen_subcommand_from certify" -l notation -d 'Adds a notation to the certification.' -r
complete -c sq -n "__fish_seen_subcommand_from certify" -l expires -d 'Makes the certification expire at TIME (as ISO 8601)' -r
complete -c sq -n "__fish_seen_subcommand_from certify" -l expires-in -d 'Makes the certification expire after DURATION (as N[ymwds]) [default: 5y]' -r
complete -c sq -n "__fish_seen_subcommand_from certify" -l private-key-store -d 'Provides parameters for private key store' -r
complete -c sq -n "__fish_seen_subcommand_from certify" -s B -l binary -d 'Emits binary data'
complete -c sq -n "__fish_seen_subcommand_from certify" -s l -l local -d 'Makes the certification a local certification'
complete -c sq -n "__fish_seen_subcommand_from certify" -l non-revocable -d 'Marks the certification as being non-revocable'
complete -c sq -n "__fish_seen_subcommand_from certify" -l allow-not-alive-certifier -d 'Don\'t fail if the certificate making the certification is not alive.'
complete -c sq -n "__fish_seen_subcommand_from certify" -l allow-revoked-certifier -d 'Don\'t fail if the certificate making the certification is revoked.'
complete -c sq -n "__fish_seen_subcommand_from certify" -s h -l help -d 'Print help information'
complete -c sq -n "__fish_seen_subcommand_from keyring; and not __fish_seen_subcommand_from filter; and not __fish_seen_subcommand_from join; and not __fish_seen_subcommand_from merge; and not __fish_seen_subcommand_from list; and not __fish_seen_subcommand_from split; and not __fish_seen_subcommand_from help" -s h -l help -d 'Print help information'
complete -c sq -n "__fish_seen_subcommand_from keyring; and not __fish_seen_subcommand_from filter; and not __fish_seen_subcommand_from join; and not __fish_seen_subcommand_from merge; and not __fish_seen_subcommand_from list; and not __fish_seen_subcommand_from split; and not __fish_seen_subcommand_from help" -f -a "filter" -d 'Joins keys into a keyring applying a filter'
complete -c sq -n "__fish_seen_subcommand_from keyring; and not __fish_seen_subcommand_from filter; and not __fish_seen_subcommand_from join; and not __fish_seen_subcommand_from merge; and not __fish_seen_subcommand_from list; and not __fish_seen_subcommand_from split; and not __fish_seen_subcommand_from help" -f -a "join" -d 'Joins keys or keyrings into a single keyring'
complete -c sq -n "__fish_seen_subcommand_from keyring; and not __fish_seen_subcommand_from filter; and not __fish_seen_subcommand_from join; and not __fish_seen_subcommand_from merge; and not __fish_seen_subcommand_from list; and not __fish_seen_subcommand_from split; and not __fish_seen_subcommand_from help" -f -a "merge" -d 'Merges keys or keyrings into a single keyring'
complete -c sq -n "__fish_seen_subcommand_from keyring; and not __fish_seen_subcommand_from filter; and not __fish_seen_subcommand_from join; and not __fish_seen_subcommand_from merge; and not __fish_seen_subcommand_from list; and not __fish_seen_subcommand_from split; and not __fish_seen_subcommand_from help" -f -a "list" -d 'Lists keys in a keyring'
complete -c sq -n "__fish_seen_subcommand_from keyring; and not __fish_seen_subcommand_from filter; and not __fish_seen_subcommand_from join; and not __fish_seen_subcommand_from merge; and not __fish_seen_subcommand_from list; and not __fish_seen_subcommand_from split; and not __fish_seen_subcommand_from help" -f -a "split" -d 'Splits a keyring into individual keys'
complete -c sq -n "__fish_seen_subcommand_from keyring; and not __fish_seen_subcommand_from filter; and not __fish_seen_subcommand_from join; and not __fish_seen_subcommand_from merge; and not __fish_seen_subcommand_from list; and not __fish_seen_subcommand_from split; and not __fish_seen_subcommand_from help" -f -a "help" -d 'Print this message or the help of the given subcommand(s)'
complete -c sq -n "__fish_seen_subcommand_from keyring; and __fish_seen_subcommand_from filter" -s o -l output -d 'Writes to FILE or stdout if omitted' -r
complete -c sq -n "__fish_seen_subcommand_from keyring; and __fish_seen_subcommand_from filter" -l userid -d 'Matches on USERID' -r
complete -c sq -n "__fish_seen_subcommand_from keyring; and __fish_seen_subcommand_from filter" -l name -d 'Matches on NAME' -r
complete -c sq -n "__fish_seen_subcommand_from keyring; and __fish_seen_subcommand_from filter" -l email -d 'Matches on email ADDRESS' -r
complete -c sq -n "__fish_seen_subcommand_from keyring; and __fish_seen_subcommand_from filter" -l domain -d 'Matches on email domain FQDN' -r
complete -c sq -n "__fish_seen_subcommand_from keyring; and __fish_seen_subcommand_from filter" -l handle -d 'Matches on (sub)key fingerprints and key ids' -r
complete -c sq -n "__fish_seen_subcommand_from keyring; and __fish_seen_subcommand_from filter" -s P -l prune-certs -d 'Removes certificate components not matching the filter'
complete -c sq -n "__fish_seen_subcommand_from keyring; and __fish_seen_subcommand_from filter" -s B -l binary -d 'Emits binary data'
complete -c sq -n "__fish_seen_subcommand_from keyring; and __fish_seen_subcommand_from filter" -l to-cert -d 'Converts any keys in the input to certificates.  Converting a key to a certificate removes secret key material from the key thereby turning it into a certificate.'
complete -c sq -n "__fish_seen_subcommand_from keyring; and __fish_seen_subcommand_from filter" -s h -l help -d 'Print help information'
complete -c sq -n "__fish_seen_subcommand_from keyring; and __fish_seen_subcommand_from join" -s o -l output -d 'Sets the output file to use' -r
complete -c sq -n "__fish_seen_subcommand_from keyring; and __fish_seen_subcommand_from join" -s B -l binary -d 'Don\'t ASCII-armor the keyring'
complete -c sq -n "__fish_seen_subcommand_from keyring; and __fish_seen_subcommand_from join" -s h -l help -d 'Print help information'
complete -c sq -n "__fish_seen_subcommand_from keyring; and __fish_seen_subcommand_from merge" -s o -l output -d 'Writes to FILE or stdout if omitted' -r
complete -c sq -n "__fish_seen_subcommand_from keyring; and __fish_seen_subcommand_from merge" -s B -l binary -d 'Emits binary data'
complete -c sq -n "__fish_seen_subcommand_from keyring; and __fish_seen_subcommand_from merge" -s h -l help -d 'Print help information'
complete -c sq -n "__fish_seen_subcommand_from keyring; and __fish_seen_subcommand_from list" -l all-userids -d 'Lists all user ids'
complete -c sq -n "__fish_seen_subcommand_from keyring; and __fish_seen_subcommand_from list" -s h -l help -d 'Print help information'
complete -c sq -n "__fish_seen_subcommand_from keyring; and __fish_seen_subcommand_from split" -s p -l prefix -d 'Writes to files with PREFIX [defaults: "FILE-" if FILE is set, or "output-" if read from stdin]' -r
complete -c sq -n "__fish_seen_subcommand_from keyring; and __fish_seen_subcommand_from split" -s B -l binary -d 'Emits binary data'
complete -c sq -n "__fish_seen_subcommand_from keyring; and __fish_seen_subcommand_from split" -s h -l help -d 'Print help information'
complete -c sq -n "__fish_seen_subcommand_from key; and not __fish_seen_subcommand_from generate; and not __fish_seen_subcommand_from password; and not __fish_seen_subcommand_from userid; and not __fish_seen_subcommand_from extract-cert; and not __fish_seen_subcommand_from adopt; and not __fish_seen_subcommand_from attest-certifications; and not __fish_seen_subcommand_from help" -s h -l help -d 'Print help information'
complete -c sq -n "__fish_seen_subcommand_from key; and not __fish_seen_subcommand_from generate; and not __fish_seen_subcommand_from password; and not __fish_seen_subcommand_from userid; and not __fish_seen_subcommand_from extract-cert; and not __fish_seen_subcommand_from adopt; and not __fish_seen_subcommand_from attest-certifications; and not __fish_seen_subcommand_from help" -f -a "generate" -d 'Generates a new key'
complete -c sq -n "__fish_seen_subcommand_from key; and not __fish_seen_subcommand_from generate; and not __fish_seen_subcommand_from password; and not __fish_seen_subcommand_from userid; and not __fish_seen_subcommand_from extract-cert; and not __fish_seen_subcommand_from adopt; and not __fish_seen_subcommand_from attest-certifications; and not __fish_seen_subcommand_from help" -f -a "password" -d 'Changes password protecting secrets'
complete -c sq -n "__fish_seen_subcommand_from key; and not __fish_seen_subcommand_from generate; and not __fish_seen_subcommand_from password; and not __fish_seen_subcommand_from userid; and not __fish_seen_subcommand_from extract-cert; and not __fish_seen_subcommand_from adopt; and not __fish_seen_subcommand_from attest-certifications; and not __fish_seen_subcommand_from help" -f -a "userid" -d 'Manages User IDs'
complete -c sq -n "__fish_seen_subcommand_from key; and not __fish_seen_subcommand_from generate; and not __fish_seen_subcommand_from password; and not __fish_seen_subcommand_from userid; and not __fish_seen_subcommand_from extract-cert; and not __fish_seen_subcommand_from adopt; and not __fish_seen_subcommand_from attest-certifications; and not __fish_seen_subcommand_from help" -f -a "extract-cert" -d 'Converts a key to a cert'
complete -c sq -n "__fish_seen_subcommand_from key; and not __fish_seen_subcommand_from generate; and not __fish_seen_subcommand_from password; and not __fish_seen_subcommand_from userid; and not __fish_seen_subcommand_from extract-cert; and not __fish_seen_subcommand_from adopt; and not __fish_seen_subcommand_from attest-certifications; and not __fish_seen_subcommand_from help" -f -a "adopt" -d 'Binds keys from one certificate to another'
complete -c sq -n "__fish_seen_subcommand_from key; and not __fish_seen_subcommand_from generate; and not __fish_seen_subcommand_from password; and not __fish_seen_subcommand_from userid; and not __fish_seen_subcommand_from extract-cert; and not __fish_seen_subcommand_from adopt; and not __fish_seen_subcommand_from attest-certifications; and not __fish_seen_subcommand_from help" -f -a "attest-certifications" -d 'Attests to third-party certifications'
complete -c sq -n "__fish_seen_subcommand_from key; and not __fish_seen_subcommand_from generate; and not __fish_seen_subcommand_from password; and not __fish_seen_subcommand_from userid; and not __fish_seen_subcommand_from extract-cert; and not __fish_seen_subcommand_from adopt; and not __fish_seen_subcommand_from attest-certifications; and not __fish_seen_subcommand_from help" -f -a "help" -d 'Print this message or the help of the given subcommand(s)'
complete -c sq -n "__fish_seen_subcommand_from key; and __fish_seen_subcommand_from generate" -s u -l userid -d 'Adds a userid to the key' -r
complete -c sq -n "__fish_seen_subcommand_from key; and __fish_seen_subcommand_from generate" -s c -l cipher-suite -d 'Selects the cryptographic algorithms for the key' -r -f -a "{rsa3k	,rsa4k	,cv25519	}"
complete -c sq -n "__fish_seen_subcommand_from key; and __fish_seen_subcommand_from generate" -l creation-time -d 'Sets the key\'s creation time to TIME (as ISO 8601)' -r
complete -c sq -n "__fish_seen_subcommand_from key; and __fish_seen_subcommand_from generate" -l expires -d 'Makes the key expire at TIME (as ISO 8601)' -r
complete -c sq -n "__fish_seen_subcommand_from key; and __fish_seen_subcommand_from generate" -l expires-in -d 'Makes the key expire after DURATION (as N[ymwds]) [default: 5y]' -r
complete -c sq -n "__fish_seen_subcommand_from key; and __fish_seen_subcommand_from generate" -l can-encrypt -d 'Adds an encryption-capable subkey [default: universal]' -r -f -a "{transport	,storage	,universal	}"
complete -c sq -n "__fish_seen_subcommand_from key; and __fish_seen_subcommand_from generate" -s e -l export -d 'Writes the key to OUTFILE' -r
complete -c sq -n "__fish_seen_subcommand_from key; and __fish_seen_subcommand_from generate" -l rev-cert -d 'Writes the revocation certificate to FILE' -r
complete -c sq -n "__fish_seen_subcommand_from key; and __fish_seen_subcommand_from generate" -l with-password -d 'Protects the key with a password'
complete -c sq -n "__fish_seen_subcommand_from key; and __fish_seen_subcommand_from generate" -l can-sign -d 'Adds a signing-capable subkey (default)'
complete -c sq -n "__fish_seen_subcommand_from key; and __fish_seen_subcommand_from generate" -l cannot-sign -d 'Adds no signing-capable subkey'
complete -c sq -n "__fish_seen_subcommand_from key; and __fish_seen_subcommand_from generate" -l can-authenticate -d 'Adds an authentication-capable subkey (default)'
complete -c sq -n "__fish_seen_subcommand_from key; and __fish_seen_subcommand_from generate" -l cannot-authenticate -d 'Adds no authentication-capable subkey'
complete -c sq -n "__fish_seen_subcommand_from key; and __fish_seen_subcommand_from generate" -l cannot-encrypt -d 'Adds no encryption-capable subkey'
complete -c sq -n "__fish_seen_subcommand_from key; and __fish_seen_subcommand_from generate" -s h -l help -d 'Print help information'
complete -c sq -n "__fish_seen_subcommand_from key; and __fish_seen_subcommand_from password" -s o -l output -d 'Writes to FILE or stdout if omitted' -r
complete -c sq -n "__fish_seen_subcommand_from key; and __fish_seen_subcommand_from password" -l clear -d 'Emit a key with unencrypted secrets'
complete -c sq -n "__fish_seen_subcommand_from key; and __fish_seen_subcommand_from password" -s B -l binary -d 'Emits binary data'
complete -c sq -n "__fish_seen_subcommand_from key; and __fish_seen_subcommand_from password" -s h -l help -d 'Print help information'
complete -c sq -n "__fish_seen_subcommand_from key; and __fish_seen_subcommand_from userid; and not __fish_seen_subcommand_from add; and not __fish_seen_subcommand_from strip; and not __fish_seen_subcommand_from help" -s h -l help -d 'Print help information'
complete -c sq -n "__fish_seen_subcommand_from key; and __fish_seen_subcommand_from userid; and not __fish_seen_subcommand_from add; and not __fish_seen_subcommand_from strip; and not __fish_seen_subcommand_from help" -f -a "add" -d 'Adds a User ID'
complete -c sq -n "__fish_seen_subcommand_from key; and __fish_seen_subcommand_from userid; and not __fish_seen_subcommand_from add; and not __fish_seen_subcommand_from strip; and not __fish_seen_subcommand_from help" -f -a "strip" -d 'Strips a User ID'
complete -c sq -n "__fish_seen_subcommand_from key; and __fish_seen_subcommand_from userid; and not __fish_seen_subcommand_from add; and not __fish_seen_subcommand_from strip; and not __fish_seen_subcommand_from help" -f -a "help" -d 'Print this message or the help of the given subcommand(s)'
complete -c sq -n "__fish_seen_subcommand_from key; and __fish_seen_subcommand_from userid; and __fish_seen_subcommand_from add" -s o -l output -d 'Writes to FILE or stdout if omitted' -r
complete -c sq -n "__fish_seen_subcommand_from key; and __fish_seen_subcommand_from userid; and __fish_seen_subcommand_from add" -s u -l userid -d 'User ID to add' -r
complete -c sq -n "__fish_seen_subcommand_from key; and __fish_seen_subcommand_from userid; and __fish_seen_subcommand_from add" -l creation-time -d 'Sets the binding signature creation time to TIME (as ISO 8601)' -r
complete -c sq -n "__fish_seen_subcommand_from key; and __fish_seen_subcommand_from userid; and __fish_seen_subcommand_from add" -l private-key-store -d 'Provides parameters for private key store' -r
complete -c sq -n "__fish_seen_subcommand_from key; and __fish_seen_subcommand_from userid; and __fish_seen_subcommand_from add" -s B -l binary -d 'Emits binary data'
complete -c sq -n "__fish_seen_subcommand_from key; and __fish_seen_subcommand_from userid; and __fish_seen_subcommand_from add" -s h -l help -d 'Print help information'
complete -c sq -n "__fish_seen_subcommand_from key; and __fish_seen_subcommand_from userid; and __fish_seen_subcommand_from strip" -s o -l output -d 'Writes to FILE or stdout if omitted' -r
complete -c sq -n "__fish_seen_subcommand_from key; and __fish_seen_subcommand_from userid; and __fish_seen_subcommand_from strip" -s u -l userid -d 'User IDs to strip' -r
complete -c sq -n "__fish_seen_subcommand_from key; and __fish_seen_subcommand_from userid; and __fish_seen_subcommand_from strip" -s B -l binary -d 'Emits binary data'
complete -c sq -n "__fish_seen_subcommand_from key; and __fish_seen_subcommand_from userid; and __fish_seen_subcommand_from strip" -s h -l help -d 'Print help information'
complete -c sq -n "__fish_seen_subcommand_from key; and __fish_seen_subcommand_from extract-cert" -s o -l output -d 'Writes to FILE or stdout if omitted' -r
complete -c sq -n "__fish_seen_subcommand_from key; and __fish_seen_subcommand_from extract-cert" -s B -l binary -d 'Emits binary data'
complete -c sq -n "__fish_seen_subcommand_from key; and __fish_seen_subcommand_from extract-cert" -s h -l help -d 'Print help information'
complete -c sq -n "__fish_seen_subcommand_from key; and __fish_seen_subcommand_from adopt" -s r -l keyring -d 'Supplies keys for use in --key.' -r
complete -c sq -n "__fish_seen_subcommand_from key; and __fish_seen_subcommand_from adopt" -s k -l key -d 'Adds the key or subkey KEY to the TARGET-KEY' -r
complete -c sq -n "__fish_seen_subcommand_from key; and __fish_seen_subcommand_from adopt" -s o -l output -d 'Writes to FILE or stdout if omitted' -r
complete -c sq -n "__fish_seen_subcommand_from key; and __fish_seen_subcommand_from adopt" -l allow-broken-crypto -d 'Allows adopting keys from certificates using broken cryptography'
complete -c sq -n "__fish_seen_subcommand_from key; and __fish_seen_subcommand_from adopt" -s B -l binary -d 'Emits binary data'
complete -c sq -n "__fish_seen_subcommand_from key; and __fish_seen_subcommand_from adopt" -s h -l help -d 'Print help information'
complete -c sq -n "__fish_seen_subcommand_from key; and __fish_seen_subcommand_from attest-certifications" -s o -l output -d 'Writes to FILE or stdout if omitted' -r
complete -c sq -n "__fish_seen_subcommand_from key; and __fish_seen_subcommand_from attest-certifications" -l none -d 'Removes all prior attestations'
complete -c sq -n "__fish_seen_subcommand_from key; and __fish_seen_subcommand_from attest-certifications" -l all -d 'Attests to all certifications [default]'
complete -c sq -n "__fish_seen_subcommand_from key; and __fish_seen_subcommand_from attest-certifications" -s B -l binary -d 'Emits binary data'
complete -c sq -n "__fish_seen_subcommand_from key; and __fish_seen_subcommand_from attest-certifications" -s h -l help -d 'Print help information'
complete -c sq -n "__fish_seen_subcommand_from inspect" -l certifications -d 'Prints third-party certifications'
complete -c sq -n "__fish_seen_subcommand_from inspect" -s h -l help -d 'Print help information'
complete -c sq -n "__fish_seen_subcommand_from encrypt" -s o -l output -d 'Writes to FILE or stdout if omitted' -r
complete -c sq -n "__fish_seen_subcommand_from encrypt" -l recipient-cert -d 'Encrypts for all recipients in CERT-RING' -r
complete -c sq -n "__fish_seen_subcommand_from encrypt" -l signer-key -d 'Signs the message with KEY' -r
complete -c sq -n "__fish_seen_subcommand_from encrypt" -l private-key-store -d 'Provides parameters for private key store' -r
complete -c sq -n "__fish_seen_subcommand_from encrypt" -l mode -d 'Selects what kind of keys are considered for encryption.' -r -f -a "{transport	,rest	,all	}"
complete -c sq -n "__fish_seen_subcommand_from encrypt" -l compression -d 'Selects compression scheme to use' -r -f -a "{none	,pad	,zip	,zlib	,bzip2	}"
complete -c sq -n "__fish_seen_subcommand_from encrypt" -s t -l time -d 'Chooses keys valid at the specified time and sets the signature\'s creation time' -r
complete -c sq -n "__fish_seen_subcommand_from encrypt" -s B -l binary -d 'Emits binary data'
complete -c sq -n "__fish_seen_subcommand_from encrypt" -s s -l symmetric -d 'Adds a password to encrypt with'
complete -c sq -n "__fish_seen_subcommand_from encrypt" -l use-expired-subkey -d 'Falls back to expired encryption subkeys'
complete -c sq -n "__fish_seen_subcommand_from encrypt" -s h -l help -d 'Print help information'
complete -c sq -n "__fish_seen_subcommand_from decrypt" -s o -l output -d 'Writes to FILE or stdout if omitted' -r
complete -c sq -n "__fish_seen_subcommand_from decrypt" -s n -l signatures -d 'Sets the threshold of valid signatures to N' -r
complete -c sq -n "__fish_seen_subcommand_from decrypt" -l signer-cert -d 'Verifies signatures with CERT' -r
complete -c sq -n "__fish_seen_subcommand_from decrypt" -l recipient-key -d 'Decrypts with KEY' -r
complete -c sq -n "__fish_seen_subcommand_from decrypt" -l private-key-store -d 'Provides parameters for private key store' -r
complete -c sq -n "__fish_seen_subcommand_from decrypt" -l session-key -d 'Decrypts an encrypted message using SESSION-KEY' -r
complete -c sq -n "__fish_seen_subcommand_from decrypt" -l dump-session-key -d 'Prints the session key to stderr'
complete -c sq -n "__fish_seen_subcommand_from decrypt" -l dump -d 'Prints a packet dump to stderr'
complete -c sq -n "__fish_seen_subcommand_from decrypt" -s x -l hex -d 'Prints a hexdump (implies --dump)'
complete -c sq -n "__fish_seen_subcommand_from decrypt" -s h -l help -d 'Print help information'
complete -c sq -n "__fish_seen_subcommand_from autocrypt; and not __fish_seen_subcommand_from decode; and not __fish_seen_subcommand_from encode-sender; and not __fish_seen_subcommand_from help" -s h -l help -d 'Print help information'
complete -c sq -n "__fish_seen_subcommand_from autocrypt; and not __fish_seen_subcommand_from decode; and not __fish_seen_subcommand_from encode-sender; and not __fish_seen_subcommand_from help" -f -a "decode" -d 'Reads Autocrypt-encoded certificates'
complete -c sq -n "__fish_seen_subcommand_from autocrypt; and not __fish_seen_subcommand_from decode; and not __fish_seen_subcommand_from encode-sender; and not __fish_seen_subcommand_from help" -f -a "encode-sender" -d 'Encodes a certificate into an Autocrypt header'
complete -c sq -n "__fish_seen_subcommand_from autocrypt; and not __fish_seen_subcommand_from decode; and not __fish_seen_subcommand_from encode-sender; and not __fish_seen_subcommand_from help" -f -a "help" -d 'Print this message or the help of the given subcommand(s)'
complete -c sq -n "__fish_seen_subcommand_from autocrypt; and __fish_seen_subcommand_from decode" -s o -l output -d 'Writes to FILE or stdout if omitted' -r
complete -c sq -n "__fish_seen_subcommand_from autocrypt; and __fish_seen_subcommand_from decode" -s B -l binary -d 'Emits binary data'
complete -c sq -n "__fish_seen_subcommand_from autocrypt; and __fish_seen_subcommand_from decode" -s h -l help -d 'Print help information'
complete -c sq -n "__fish_seen_subcommand_from autocrypt; and __fish_seen_subcommand_from encode-sender" -s o -l output -d 'Writes to FILE or stdout if omitted' -r
complete -c sq -n "__fish_seen_subcommand_from autocrypt; and __fish_seen_subcommand_from encode-sender" -l email -d 'Sets the address [default: primary userid]' -r
complete -c sq -n "__fish_seen_subcommand_from autocrypt; and __fish_seen_subcommand_from encode-sender" -l prefer-encrypt -d 'Sets the prefer-encrypt attribute' -r -f -a "{nopreference	,mutual	}"
complete -c sq -n "__fish_seen_subcommand_from autocrypt; and __fish_seen_subcommand_from encode-sender" -s h -l help -d 'Print help information'
