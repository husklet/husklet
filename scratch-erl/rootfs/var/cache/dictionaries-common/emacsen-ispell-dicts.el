;;; This file is part of the dictionaries-common package.
;;; It has been automatically generated.
;;; DO NOT EDIT!

;; Adding hunspell dicts

(add-to-list 'debian-hunspell-only-dictionary-alist
  '("english_american"
     "[a-zA-Z]"
     "[^a-zA-Z]"
     "[']"
     nil
     ("-d" "en_US")
     nil
     utf-8))
(add-to-list 'debian-hunspell-only-dictionary-alist
  '("american"
     "[a-zA-Z\303\251\303\263\303\266\303\242\303\264\303\205\303\247\303\250\303\256\303\252\303\240\303\257\303\274\303\244\303\261]"
     "[^a-zA-Z\303\251\303\263\303\266\303\242\303\264\303\205\303\247\303\250\303\256\303\252\303\240\303\257\303\274\303\244\303\261]"
     "[0123456789’]"
     nil
     ("-d" "en_US")
     nil
     utf-8))



;; No emacsen-aspell-equivs entries were found


;; An alist that will try to map hunspell locales to emacsen names

(setq debian-hunspell-equivs-alist '(
     ("en_US" "american")
))

;; Get default value for debian-hunspell-dictionary. Will be used if
;; spellchecker is hunspell and ispell-local-dictionary is not set.
;; We need to get it here, after debian-hunspell-equivs-alist is loaded

(setq debian-hunspell-dictionary (debian-ispell-get-hunspell-default))

