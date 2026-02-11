# dxfeed Implementation Progress

## Completed Steps
- [x] Step 1: Crate scaffold and domain enums (14 tests)
- [x] Step 2: Core data model (21 tests cumulative)
- [x] Step 3: Filter serde config structs (30 tests cumulative)
- [x] Step 4: CompiledPatternSet + validated FilterConfig (60 tests cumulative)
- [x] Step 5: Filter evaluate() function (117 tests cumulative)

## Current Step
Step 6: Spot line parser

## Applied Fixes (from review)
- [x] Fix #1: Added CQ/ITU zone checks to eval_entity() and eval_geo() (Step 5)
- [x] Fix #2: Applied case_insensitive to glob patterns in CompiledPatternSet (Step 4)
- [x] Fix #3: Split UnknownFieldPolicy::Neutral for allowlist vs denylist context (Step 5)

## Remaining Deferred Fixes
- [ ] Fix #4: Key skimmer TimeWindowedIndex by dx_call (Step 10)
- [ ] Fix #5: Latin-1 encoding handling on telnet connections (Step 12)
- [ ] Fix #6: DIG bucket default = 100 Hz (Step 7 or 9)
- [ ] Fix #7: Withdraw events on TTL expiry (Step 11)
- [ ] Fix #8: Parse freq string directly to u64 Hz (Step 6)
- [ ] Fix #9: Rename valid_required_count -> valid_required_distinct_skimmers (Step 10)

## Notes
- thiserror = "2" (latest)
- regex::Regex does implement Clone in regex 1.x, so ContentFilters Clone is fine
- MembershipRuleSerde uses Vec (not BTreeSet) but has Ord derived for flexibility
- MembershipRuleSerde::Require < MembershipRuleSerde::Deny by discriminant order
