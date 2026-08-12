void hl_lint_extended_environment_fixture(void) {
    secure_getenv("ONE");
    __secure_getenv("TWO");
    _dupenv_s(0, 0, "THREE");
    GetEnvironmentVariableA("FOUR", 0, 0);
    GetEnvironmentVariableW(L"FIVE", 0, 0);
}
