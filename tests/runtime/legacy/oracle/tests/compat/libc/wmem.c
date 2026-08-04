// wide-char memory/string ops: wmemcpy/wcschr/wcslen/wcscmp/wcsncpy. Portable verdicts.
#include <stdio.h>
#include <wchar.h>

int main(void) {
    wchar_t a[8]; wmemset(a, L'x', 8);
    int d1 = a[0] == L'x' && a[7] == L'x';
    wchar_t src[] = L"hello";
    wchar_t dst[8]; wmemcpy(dst, src, 6);
    int d2 = wcscmp(dst, L"hello") == 0 && wcslen(dst) == 5;
    int d3 = wcschr(src, L'l') == src + 2 && wcsrchr(src, L'l') == src + 3;
    wchar_t nc[8]; wcsncpy(nc, L"ab", 8);
    int d4 = nc[0] == L'a' && nc[2] == 0 && nc[7] == 0;
    int d5 = wcsncmp(L"abcx", L"abcy", 3) == 0;
    printf("wmem d1=%d d2=%d d3=%d d4=%d d5=%d\n", d1, d2, d3, d4, d5);
    return 0;
}
