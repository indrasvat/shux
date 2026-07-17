# Task 078 — frozen RED contract lane transcript

Command: make test-lens-gate-contract
Expected: ALL cases FAIL until tasks 081 (runner) + 082 (verdict) build `shux lens gate`.
Each case goes GREEN by BUILDING the feature — never by editing the assertion (freeze guard forbids it).

```
        FAIL  (1/5) shux::lens_gate_contract gate_emits_conforming_report_json
        FAIL  (2/5) shux::lens_gate_contract gate_exit_code_matches_rolled_up_status
        FAIL  (3/5) shux::lens_gate_contract gate_help_documents_the_verb
        FAIL  (4/5) shux::lens_gate_contract gate_missing_golden_fails_ci_safe
        FAIL  (5/5) shux::lens_gate_contract gate_update_is_refused_in_ci_mode
     Summary  5 tests run: 0 passed, 5 failed, 0 skipped
    test gate_emits_conforming_report_json ... FAILED
    test gate_exit_code_matches_rolled_up_status ... FAILED
    test gate_help_documents_the_verb ... FAILED
    test gate_missing_golden_fails_ci_safe ... FAILED
    test gate_update_is_refused_in_ci_mode ... FAILED
    test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 4 filtered out; finished in 0.01s
    test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 4 filtered out; finished in 0.03s
make: *** [test-lens-gate-contract] Error 100
```

Retirement map:
- gate_emits_conforming_report_json      → RETIRED BY 081 (runner) + 082 (report)
- gate_exit_code_matches_rolled_up_status → RETIRED BY 082
- gate_missing_golden_fails_ci_safe       → RETIRED BY 082
- gate_update_is_refused_in_ci_mode       → RETIRED BY 082
- gate_help_documents_the_verb            → RETIRED BY 082
