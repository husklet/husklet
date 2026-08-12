void hl_lint_platform_debug_fixture(void) {
    OutputDebugStringA("one");
    OutputDebugStringW(L"two");
    NSLog(@"three");
    os_log(0, "four");
    syslog(0, "five");
}
