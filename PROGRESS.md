# dxfeed Implementation Progress

## Completed Steps
(none yet)

## Current Step
Step 1: Crate Scaffold and Domain Enums

## Deferred Fixes
- [ ] Fix #1: Add CQ/ITU zone checks to eval_entity() and eval_geo()
- [ ] Fix #2: Apply case_insensitive to glob patterns in CompiledPatternSet
- [ ] Fix #3: Split UnknownFieldPolicy::Neutral for allowlist vs denylist
- [ ] Fix #4: Key skimmer TimeWindowedIndex by dx_call
- [ ] Fix #5: Latin-1 encoding handling on telnet connections
- [ ] Fix #6: DIG bucket default = 100 Hz
- [ ] Fix #7: Withdraw events on TTL expiry
- [ ] Fix #8: Parse freq string directly to u64 Hz
- [ ] Fix #9: Rename valid_required_count -> valid_required_distinct_skimmers

## Notes
- thiserror = "2" (latest, plan says "2", reference code said "1" but we go with 2)
- regex::Regex does implement Clone in regex 1.x, so ContentFilters Clone is fine
- MembershipRuleSerde needs Ord for BTreeSet - derive it
