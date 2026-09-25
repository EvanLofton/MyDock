# T2 WS_EX_NOACTIVATE 点击不夺焦点验证

| 变体 | 点击前焦点 | 点击后焦点 | 焦点是否被夺 | Dock 是否收到点击 | 判定 |
|---|---|---|---|---|---|
| V1 NOACTIVATE | #ProbeHolder2:FOCUS-HOLDER | #ProbeDock:DOCK | true | 1 | **FAIL** |
| V1b NOACTIVATE (click-focus) | #ProbeHolder2:FOCUS-HOLDER | #ProbeDock:DOCK | true | 1 | **FAIL** |
| V2 NOACTIVATE+MA_NOACTIVATE | #ProbeHolder2:FOCUS-HOLDER | #ProbeHolder2:FOCUS-HOLDER | false | 1 | **PASS** |
| V2b NOACT+MA (click-focus) | #ProbeHolder2:FOCUS-HOLDER | #ProbeHolder2:FOCUS-HOLDER | false | 1 | **PASS** |
| V4 MA_NOACTIVATE only | #ProbeHolder2:FOCUS-HOLDER | #ProbeHolder2:FOCUS-HOLDER | false | 1 | **PASS** |
| V3 control (no protection) | #ProbeHolder2:FOCUS-HOLDER | #ProbeDock:DOCK | true | 1 | **PASS** |
| V5 NOACTIVATE (other proc) | #ProbeExternalHolder:EXTERNAL-HOLDER | #ProbeExternalHolder:EXTERNAL-HOLDER | false | 1 | **PASS** |
| V6 NOACT+MA (other proc) | #ProbeExternalHolder:EXTERNAL-HOLDER | #ProbeExternalHolder:EXTERNAL-HOLDER | false | 1 | **PASS** |
| V7 control (other proc) | #ProbeExternalHolder:EXTERNAL-HOLDER | #ProbeDock:DOCK | true | 1 | **PASS** |

**总体结论：存在不符合预期的用例，需人工复查**
