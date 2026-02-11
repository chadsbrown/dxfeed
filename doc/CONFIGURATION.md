# Configuration Reference

This document covers every configuration option in dxfeed. All configuration
is hot-reloadable at runtime (filter, aggregator, skimmer quality) without
restarting source connections.

Configuration is split into four areas:

| Section | Rust type | Purpose |
|---------|-----------|---------|
| [Filter](#filter-configuration) | `FilterConfigSerde` | Declarative rules for which spots to accept or reject |
| [Aggregator](#aggregator-configuration) | `AggregatorConfig` | Deduplication, frequency bucketing, TTL, normalization |
| [Skimmer Quality](#skimmer-quality-configuration) | `SkimmerQualityConfig` | CT1BOH-style skimmer gating and classification |
| [Source](#source-configuration) | `SourceConfig` | Cluster connection settings, reconnect backoff |

---

## Filter Configuration

**Type:** `FilterConfigSerde` (`src/filter/config.rs`)

Filters are defined as JSON (or constructed programmatically) and compiled
into an optimized runtime form via `validate_and_compile()`. Compilation
validates all values and pre-compiles glob/regex patterns.

### Top-level fields

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `max_age_secs` | `u64` | `900` (15 min) | Maximum spot age in seconds. Spots older than this at evaluation time are dropped. Must be > 0. |
| `freq_sanity_hz` | `[u64, u64]` or `null` | `[1800000, 54000000]` | Frequency sanity bounds (min, max) in Hz. Spots outside this range are dropped. Set to `null` to disable. |
| `unknown_policy` | `UnknownFieldPolicy` | `"Neutral"` | How to handle unknown/missing field values during filter evaluation. See [Unknown Field Policy](#unknown-field-policy). |

### RF Filters (`rf`)

**Type:** `RfFiltersSerde`

Controls which bands, modes, and frequency ranges are accepted.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `band_allow` | `[Band]` | `[]` (all) | Allowlist of bands. Empty = allow all bands (subject to deny). |
| `band_deny` | `[Band]` | `[]` | Denylist of bands. Deny is checked first. |
| `mode_allow` | `[DxMode]` | `[]` (all) | Allowlist of modes. Empty = allow all modes (subject to deny). |
| `mode_deny` | `[DxMode]` | `[]` | Denylist of modes. Deny is checked first. |
| `freq_allow_hz` | `[[u64, u64]]` | `[]` (all) | Frequency allow ranges in Hz. Each entry is `[min, max]`. Empty = allow all. Spot must fall within at least one range. |
| `freq_deny_hz` | `[[u64, u64]]` | `[]` | Frequency deny ranges in Hz. Each entry is `[min, max]`. If the spot frequency falls in any deny range, it is dropped. |
| `profiles` | `[RfProfile]` | `[]` | Named frequency profiles. If non-empty, the spot must match at least one profile. See [RF Profiles](#rf-profiles). |
| `mode_policy` | `ModePolicy` | `"PreferUpstreamFallbackInfer"` | How operating mode is determined. See [Domain Types](#domain-types-reference). |

**Evaluation order:** band deny > band allow > mode deny > mode allow > freq deny > freq allow > profiles.

#### RF Profiles

An RF profile is a named collection of frequency ranges. When profiles are
configured, a spot's frequency must fall within at least one range of at least
one profile to pass.

```json
{
  "rf": {
    "profiles": [
      {
        "name": "CW Contest",
        "ranges_hz": [[7000000, 7040000], [14000000, 14070000]]
      },
      {
        "name": "SSB Contest",
        "ranges_hz": [[7150000, 7300000], [14150000, 14350000]]
      }
    ]
  }
}
```

### DX Callsign Filters (`dx`)

**Type:** `CallsignFiltersSerde`

Filters on the DX station's callsign (the station being spotted).

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `allow` | `PatternSet` | empty | Allowlist patterns. If non-empty, only matching callsigns pass. |
| `deny` | `PatternSet` | empty | Denylist patterns. Matching callsigns are dropped. Deny is checked first. |
| `normalization` | `CallsignNormalization` | see below | How callsigns are normalized before matching. |

#### Callsign Normalization

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `uppercase` | `bool` | `true` | Convert callsigns to uppercase before matching. |
| `portable_policy` | `PortablePolicy` | `"KeepAsIs"` | How to handle portable suffixes (`/P`, `/M`, `/QRP`, etc.). |
| `strip_non_alnum` | `bool` | `false` | Strip non-alphanumeric characters before matching. |

### Spotter Filters (`spotter`)

**Type:** `SpotterFiltersSerde`

Filters on who reported the spot (the spotter/skimmer).

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `callsign` | `CallsignFiltersSerde` | empty | Spotter callsign allow/deny patterns, same structure as `dx`. |
| `source_allow` | `[string]` | `[]` (all) | Allowlist of source IDs. Empty = allow all sources. |
| `source_deny` | `[string]` | `[]` | Denylist of source IDs. |
| `allow_human` | `bool` | `true` | Accept spots from human operators. |
| `allow_skimmer` | `bool` | `true` | Accept spots from automated skimmers. |
| `allow_unknown_kind` | `bool` | `true` | Accept spots where the originator kind could not be determined. |

### Content Filters (`content`)

**Type:** `ContentFiltersSerde`

Filters on spot comment text and content characteristics.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `comment_allow` | `PatternSet` | empty | Allowlist patterns for comment text. If non-empty, only spots with matching comments pass. |
| `comment_deny` | `PatternSet` | empty | Denylist patterns for comment text. Matching comments are dropped. |
| `cq_only` | `bool` | `false` | Only accept spots that are calling CQ. Checks the skimmer CQ flag and scans comments for the word "CQ". |
| `suppress_split_qsy` | `bool` | `false` | Drop spots whose comments contain split/QSY keywords (`UP`, `QSX`, `SPLIT`, `QSY`). |
| `require_comment_when_allowlist_present` | `bool` | `true` | When `comment_allow` is non-empty, drop spots that have no comment at all (rather than letting them through). |

### Correlation Filters (`correlation`)

**Type:** `CorrelationFiltersSerde`

Require multiple independent confirmations before accepting a spot. All
minimums are clamped to at least 1 during compilation.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `min_unique_originators` | `u8` | `1` | Minimum number of distinct spotters/skimmers that must have reported the spot. |
| `originators_human_only` | `bool` | `false` | When counting unique originators, only count human operators (ignore skimmers). |
| `min_unique_sources` | `u8` | `1` | Minimum number of distinct upstream source connections that reported the spot. |
| `min_observations` | `u8` | `1` | Minimum total number of observations (reports) for the spot. |
| `window_secs` | `u64` | `180` (3 min) | Time window in seconds for counting observations. Clamped to at least 1. |
| `min_repeats_same_key` | `u8` | `1` | Minimum number of observations with the exact same spot key. |

### Skimmer Metric Filters (`skimmer`)

**Type:** `SkimmerMetricFiltersSerde`

Filters on skimmer-specific metrics. These filters only apply to spots
originating from skimmers (`OriginatorKind::Skimmer`); human-originated spots
are not affected.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `snr_db` | `[i8, i8]` or `null` | `null` | Acceptable SNR range `[min, max]` in dB. Reversed ranges are auto-corrected. `null` = no SNR filtering. |
| `wpm` | `[u16, u16]` or `null` | `null` | Acceptable WPM range `[min, max]`. Reversed ranges are auto-corrected. `null` = no WPM filtering. |
| `drop_dupes` | `bool` | `false` | Drop spots flagged as duplicates by the skimmer. |
| `require_cq` | `bool` | `false` | Only accept spots where the skimmer detected a CQ call. |

### Geographic Filters (`geo`)

**Type:** `GeoFiltersSerde`

Filters on resolved geographic/entity data. Requires an `EntityResolver` to
be configured for geo data to be available.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `dx` | `EntityFilters` | empty | Geographic filters applied to the DX station (the station being spotted). |
| `spotter` | `EntityFilters` | empty | Geographic filters applied to the spotter/skimmer. |
| `require_resolvers` | `bool` | `false` | Drop spots where no entity data could be resolved, regardless of other geo filters. |

#### Entity Filters

Each of `geo.dx` and `geo.spotter` has the same structure:

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `continent_allow` | `[Continent]` | `[]` | Allowlist of continents. |
| `continent_deny` | `[Continent]` | `[]` | Denylist of continents. |
| `cq_zone_allow` | `[u8]` | `[]` | Allowlist of CQ zones (1-40). |
| `cq_zone_deny` | `[u8]` | `[]` | Denylist of CQ zones. |
| `itu_zone_allow` | `[u8]` | `[]` | Allowlist of ITU zones (1-90). |
| `itu_zone_deny` | `[u8]` | `[]` | Denylist of ITU zones. |
| `entity_allow` | `[string]` | `[]` | Allowlist of DXCC entity names (e.g., `"Japan"`, `"Fed. Rep. of Germany"`). |
| `entity_deny` | `[string]` | `[]` | Denylist of DXCC entity names. |
| `country_allow` | `[string]` | `[]` | Allowlist of country names. |
| `country_deny` | `[string]` | `[]` | Denylist of country names. |
| `state_allow` | `[string]` | `[]` | Allowlist of US state/Canadian province codes (e.g., `"CT"`, `"ON"`). |
| `state_deny` | `[string]` | `[]` | Denylist of state codes. |
| `grid_allow` | `PatternSet` | empty | Allowlist patterns for Maidenhead grid squares (e.g., `"FN*"`, `"JO*"`). |
| `grid_deny` | `PatternSet` | empty | Denylist patterns for grid squares. |

### Enrichment Filters (`enrichment`)

**Type:** `EnrichmentFiltersSerde`

Filters on enrichment data (LoTW, master DB, callbook, club memberships).
Requires an `EnrichmentResolver` to be configured.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `lotw` | `TriState` | `"Any"` | LoTW user filter. `"RequireTrue"` = only LoTW users, `"RequireFalse"` = exclude LoTW users. |
| `in_master_db` | `TriState` | `"Any"` | Super Check Partial / contest master DB filter. |
| `in_callbook` | `TriState` | `"Any"` | Callbook presence filter. |
| `membership` | `[MembershipRule]` | `[]` | Club membership requirements. Each rule is `{"Require": "TAG"}` or `{"Deny": "TAG"}`. All rules must pass. |

Example:

```json
{
  "enrichment": {
    "lotw": "RequireTrue",
    "membership": [
      {"Require": "FOC"},
      {"Deny": "SPAM"}
    ]
  }
}
```

### Pattern Matching Reference

**Type:** `PatternSetSerde`

Pattern sets are used for callsign, comment, and grid matching throughout the
filter configuration. Each set contains globs, regexes, or both. A match
against any single pattern in the set is sufficient.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `globs` | `[string]` | `[]` | Glob patterns. `*` matches any sequence of characters, `?` matches one character, `[abc]` matches a character class. |
| `regexes` | `[string]` | `[]` | Regular expressions (Rust `regex` crate syntax). |
| `case_insensitive` | `bool` | `false` | Make all patterns in this set case-insensitive. |

**Glob syntax:** Uses the `globset` crate. `*` matches any characters
(including none), `?` matches exactly one character, `[...]` matches a
character class. There is no recursive `**` — patterns match flat strings
(callsigns, comments), not file paths.

**Regex syntax:** Uses the Rust `regex` crate. Supports full regex syntax
including `^`, `$`, character classes, alternation, and lookahead. When
`case_insensitive` is true, each regex is wrapped in `(?i:...)`.

Example:

```json
{
  "dx": {
    "allow": {
      "globs": ["JA*", "JH*", "JR*"],
      "regexes": ["^(VK|ZL)"],
      "case_insensitive": true
    },
    "deny": {
      "globs": ["BEACON*"]
    }
  }
}
```

### Filter Evaluation Order

Filters are evaluated in a fixed order. Evaluation stops at the first failure
(short-circuit). The order is:

1. **Age** — `max_age_secs`
2. **Frequency sanity** — `freq_sanity_hz`
3. **RF** — band deny/allow, mode deny/allow, freq deny/allow, profiles
4. **DX callsign** — deny, then allow
5. **Spotter/originator** — originator kind, source deny/allow, callsign deny/allow
6. **Content** — split/QSY suppression, CQ-only, comment deny/allow
7. **Correlation** — minimum originators, sources, observations
8. **Skimmer metrics** — SNR, WPM, dupes, CQ (skimmer spots only)
9. **Geographic** — continent, CQ zone, ITU zone, entity, country, state, grid
10. **Enrichment** — LoTW, master DB, callbook, memberships

**Note:** Skimmer quality gating (`SkimmerQualityConfig`) is applied by the
aggregator *before* the filter pipeline runs.

### Unknown Field Policy

**Type:** `UnknownFieldPolicy`

Controls what happens when a filter references a field that has no value
(e.g., geo filters when no entity resolver is configured).

| Value | Behavior |
|-------|----------|
| `"Neutral"` (default) | **Allowlists:** unknown = drop (field not proven to be in the set). **Denylists:** unknown = pass (field not proven to be denied). This is the recommended default. |
| `"FailClosed"` | Unknown fields always cause the spot to be dropped. Use this for maximum strictness. |
| `"FailOpen"` | Unknown fields always pass. Use this to avoid dropping spots when enrichment data is unavailable. |

### Allow/Deny Semantics

All allow/deny pairs follow the same logic:

1. If the **deny** set is non-empty and the value matches, the spot is **dropped**.
2. If the **allow** set is non-empty and the value does **not** match, the spot is **dropped**.
3. If the **allow** set is **empty**, all values are accepted (subject to deny).
4. Deny is always checked before allow.

---

## Aggregator Configuration

**Type:** `AggregatorConfig` (`src/aggregator/core.rs`)

Controls how observations are aggregated into spots.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `freq_bucket` | `FreqBucketConfig` | see below | Frequency bucketing widths per mode. |
| `callsign_norm` | `CallsignNormalization` | see below | Callsign normalization rules (shared with filter config). |
| `dedupe` | `DedupeConfig` | see below | Deduplication and emission control. |
| `spot_ttl` | `Duration` | `900s` (15 min) | Time-to-live for spots. Spots not updated within this window are evicted and a `Withdraw` event is emitted. |
| `max_observations_per_spot` | `usize` | `20` | Maximum number of recent observations stored per spot. Oldest are discarded when the limit is reached. |
| `mode_policy` | `ModePolicy` | `"PreferUpstreamFallbackInfer"` | How operating mode is determined from spot data. |
| `max_spots` | `usize` | `50000` | Hard cap on total spots in the table. When exceeded, the oldest spot is evicted. |

### Frequency Bucketing (`freq_bucket`)

**Type:** `FreqBucketConfig`

Two observations are considered the same spot if they have the same
normalized callsign, band, mode, and frequency bucket. The bucket size
varies by mode:

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `cw_bucket_hz` | `u64` | `10` | CW frequency bucket width in Hz. Two CW spots within 10 Hz of each other (after bucketing) are the same spot. |
| `ssb_bucket_hz` | `u64` | `1000` | SSB frequency bucket width in Hz. |
| `dig_bucket_hz` | `u64` | `100` | Digital mode frequency bucket width in Hz. |

### Deduplication (`dedupe`)

**Type:** `DedupeConfig`

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `emit_updates` | `bool` | `false` | When `true`, emit `Update` events when an already-emitted spot receives new observations. When `false`, only `New` and `Withdraw` events are emitted. |
| `min_update_interval` | `Duration` or `null` | `null` | Minimum time between `Update` emissions for the same spot. Suppresses rapid-fire updates. `null` = no cooldown (every qualifying update emits). |
| `detect_qsy` | `bool` | `true` | When `true`, if a station is spotted at a new frequency bucket on the same band/mode, the old spot is withdrawn and a new one is emitted (`[Withdraw, New]`). |

---

## Skimmer Quality Configuration

**Type:** `SkimmerQualityConfig` (`src/skimmer/config.rs`)

Implements CT1BOH/AR-Cluster-style skimmer quality classification. Spots
from skimmers are classified as Valid, Busted, Qsy, or Unknown based on
corroboration from multiple independent skimmers.

### Gating controls

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `enabled` | `bool` | `true` | Master enable/disable for the quality engine. |
| `gate_skimmer_output` | `bool` | `true` | When `true`, block skimmer spots that don't pass quality gating. |
| `allow_valid` | `bool` | `true` | Allow spots classified as `Valid` (corroborated by enough skimmers). |
| `allow_qsy` | `bool` | `false` | Allow spots classified as `Qsy` (station moved frequency). |
| `allow_unknown` | `bool` | `false` | Allow spots classified as `Unknown` (insufficient data). |
| `allow_busted` | `bool` | `false` | Allow spots classified as `Busted` (likely decoding error). |
| `apply_only_to_skimmer` | `bool` | `true` | Only apply gating to skimmer-originated spots. Human spots always pass. |

### Computation toggles

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `compute_valid` | `bool` | `true` | Enable Valid classification (corroboration check). |
| `compute_busted` | `bool` | `true` | Enable Busted classification (similar-call detection). |
| `compute_qsy` | `bool` | `true` | Enable Qsy classification (frequency-change detection). |

### Valid detection tuning

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `valid_required_distinct_skimmers` | `u8` | `3` | Number of distinct skimmers required to corroborate a spot for `Valid` classification. |
| `valid_freq_window_hz` | `i64` | `300` | Frequency window in Hz. Reports within this range of each other are considered corroborating. |
| `lookback_window` | `Duration` (secs) | `180s` (3 min) | Time window for counting corroborating observations. Serialized as seconds. |

### Busted detection tuning

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `busted_freq_window_hz` | `i64` | `100` | Frequency window in Hz for busted-call detection. |
| `similar_call_max_edit_distance` | `u8` | `1` | Maximum Levenshtein edit distance for a call to be considered "similar" to a verified call (and thus likely busted). |

### QSY detection tuning

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `qsy_freq_window_hz` | `i64` | `400` | Frequency difference in Hz beyond which a previously-verified call is considered to have QSY'd. |

### Other

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `max_tracked_observations` | `usize` | `100000` | Hard cap on total tracked skimmer observations to prevent unbounded memory growth. |

### Quality Tags

| Tag | Meaning |
|-----|---------|
| `Valid` | Corroborated by the required number of distinct skimmers within the frequency/time window. |
| `Busted` | The reported callsign is similar (by edit distance) to a verified call at a nearby frequency. Likely a decoding error. |
| `Qsy` | A previously verified call is now being reported at a significantly different frequency. |
| `Unknown` | Insufficient data to classify. Typically a single skimmer report with no corroboration yet. |

---

## Source Configuration

**Type:** `SourceConfig` (`src/source/supervisor.rs`)

Currently only cluster (telnet) sources are supported.

### Cluster Source (`ClusterSourceConfig`)

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `host` | `string` | (required) | DX cluster hostname or IP address. |
| `port` | `u16` | (required) | TCP port (typically 7300 or 23). |
| `callsign` | `string` | (required) | Your callsign for login. |
| `password` | `string` or `null` | `null` | Optional password for protected nodes. |
| `source_id` | `SourceId` | (required) | Unique identifier for this source connection (used in events and filters). |
| `login_timeout` | `Duration` | `30s` | Timeout for the login sequence to complete. |
| `read_timeout` | `Duration` | `300s` (5 min) | Inactivity timeout. If no data is received for this duration, the connection is considered hung and is dropped. |
| `originator_policy` | `OriginatorPolicy` | `"Auto"` | How to classify spot originators. See below. |

#### Originator Policy

| Value | Description |
|-------|-------------|
| `Auto` (default) | Infer per-spot from the spotter's callsign pattern. Callsigns with a numeric suffix (e.g., `W3LPL-2`, `DK8JP-1`) are classified as skimmers; all others as human. |
| `AllSkimmer` | Force all spots from this connection to `Skimmer`. Use for RBN (Reverse Beacon Network) connections. |
| `AllHuman` | Force all spots from this connection to `Human`. Use for cluster nodes that don't carry skimmer spots. |

### Backoff Configuration (`BackoffConfig`)

Controls automatic reconnection behavior when a source connection fails.

| Field | Type | Default | Description |
|-------|------|---------|-------------|
| `initial` | `Duration` | `2s` | Initial delay before the first reconnect attempt. |
| `max` | `Duration` | `120s` (2 min) | Maximum delay between reconnect attempts. |
| `multiplier` | `f64` | `2.0` | Multiplier applied to the delay after each consecutive failure. |
| `jitter_factor` | `f64` | `0.25` | Random jitter factor. `0.0` = no jitter, `0.25` = up to 25% additional random delay. Jitter prevents thundering-herd reconnection storms. |

On a clean disconnect (server EOF), backoff is reset and reconnection happens
after a 1-second pause.

---

## Domain Types Reference

### Band

Amateur radio frequency bands. Used in `rf.band_allow` and `rf.band_deny`.

| Value | Band |
|-------|------|
| `"B160"` | 160 meters (1.8 MHz) |
| `"B80"` | 80 meters (3.5 MHz) |
| `"B60"` | 60 meters (5 MHz) |
| `"B40"` | 40 meters (7 MHz) |
| `"B30"` | 30 meters (10 MHz) |
| `"B20"` | 20 meters (14 MHz) |
| `"B17"` | 17 meters (18 MHz) |
| `"B15"` | 15 meters (21 MHz) |
| `"B12"` | 12 meters (24 MHz) |
| `"B10"` | 10 meters (28 MHz) |
| `"B6"` | 6 meters (50 MHz) |
| `"B2"` | 2 meters (144 MHz) |
| `"Unknown"` | Frequency does not map to a known band |

### DxMode

Operating modes. Used in `rf.mode_allow` and `rf.mode_deny`.

| Value | Mode |
|-------|------|
| `"CW"` | Continuous Wave (Morse code) |
| `"SSB"` | Single Sideband (voice) |
| `"DIG"` | Digital modes (FT8, FT4, RTTY, PSK, etc.) |
| `"AM"` | Amplitude Modulation |
| `"FM"` | Frequency Modulation |
| `"Unknown"` | Mode could not be determined |

### Continent

ITU continents. Used in `geo.dx.continent_allow`, etc.

| Value | Continent |
|-------|-----------|
| `"AF"` | Africa |
| `"AN"` | Antarctica |
| `"AS"` | Asia |
| `"EU"` | Europe |
| `"NA"` | North America |
| `"OC"` | Oceania |
| `"SA"` | South America |
| `"Unknown"` | Could not be resolved |

### OriginatorKind

| Value | Description |
|-------|-------------|
| `"Human"` | Human operator |
| `"Skimmer"` | Automated CW/digital skimmer |
| `"Unknown"` | Could not be determined |

### ModePolicy

Controls how operating mode is determined from spot data.

| Value | Description |
|-------|-------------|
| `"TrustUpstream"` | Use whatever mode the cluster/skimmer reports. |
| `"InferFromFrequency"` | Ignore upstream mode; infer from frequency using the band plan. |
| `"PreferUpstreamFallbackInfer"` (default) | Use upstream mode if present, otherwise infer from frequency. |

### PortablePolicy

Controls how portable suffixes on callsigns are handled during normalization.

| Value | Description |
|-------|-------------|
| `"KeepAsIs"` (default) | Leave callsigns unchanged. `W1AW/P` stays `W1AW/P`. |
| `"StripCommonSuffixes"` | Strip common suffixes like `/P`, `/M`, `/QRP`. |
| `"StripAllAfterSlash"` | Strip everything after the first `/`. `W1AW/P` becomes `W1AW`. |

### TriState

Three-state filter for boolean enrichment flags.

| Value | Description |
|-------|-------------|
| `"Any"` (default) | No filter; accept regardless of value. |
| `"RequireTrue"` | Only accept if the flag is `true`. |
| `"RequireFalse"` | Only accept if the flag is `false`. |

### UnknownFieldPolicy

See [Unknown Field Policy](#unknown-field-policy) in the filter section.

| Value | Description |
|-------|-------------|
| `"Neutral"` (default) | Allowlists drop unknown, denylists pass unknown. |
| `"FailClosed"` | Unknown always drops. |
| `"FailOpen"` | Unknown always passes. |

### SpotEventKind

Lifecycle state of a spot event.

| Value | Description |
|-------|-------------|
| `New` | First time this spot passes filters and is emitted. |
| `Update` | Meaningful change to an already-emitted spot (requires `emit_updates: true`). |
| `Withdraw` | Spot removed due to TTL expiry, QSY detection, or capacity eviction. |

---

## Complete Filter JSON Example

This example shows all filter sections with representative values:

```json
{
  "max_age_secs": 900,
  "freq_sanity_hz": [1800000, 54000000],
  "unknown_policy": "Neutral",
  "rf": {
    "band_allow": [],
    "band_deny": ["B160", "B6"],
    "mode_allow": ["CW", "SSB", "DIG"],
    "mode_deny": [],
    "freq_allow_hz": [],
    "freq_deny_hz": [[10100000, 10150000]],
    "profiles": [],
    "mode_policy": "PreferUpstreamFallbackInfer"
  },
  "dx": {
    "allow": {
      "globs": [],
      "regexes": [],
      "case_insensitive": false
    },
    "deny": {
      "globs": ["BEACON*"],
      "regexes": ["^(PIRATE|TEST)\\d"],
      "case_insensitive": true
    },
    "normalization": {
      "uppercase": true,
      "portable_policy": "KeepAsIs",
      "strip_non_alnum": false
    }
  },
  "spotter": {
    "callsign": {
      "allow": { "globs": [], "regexes": [], "case_insensitive": false },
      "deny": { "globs": [], "regexes": [], "case_insensitive": false },
      "normalization": {
        "uppercase": true,
        "portable_policy": "KeepAsIs",
        "strip_non_alnum": false
      }
    },
    "source_allow": [],
    "source_deny": [],
    "allow_human": true,
    "allow_skimmer": true,
    "allow_unknown_kind": true
  },
  "content": {
    "comment_allow": { "globs": [], "regexes": [], "case_insensitive": false },
    "comment_deny": { "globs": [], "regexes": [], "case_insensitive": false },
    "cq_only": false,
    "suppress_split_qsy": false,
    "require_comment_when_allowlist_present": true
  },
  "correlation": {
    "min_unique_originators": 1,
    "originators_human_only": false,
    "min_unique_sources": 1,
    "min_observations": 1,
    "window_secs": 180,
    "min_repeats_same_key": 1
  },
  "skimmer": {
    "snr_db": null,
    "wpm": null,
    "drop_dupes": false,
    "require_cq": false
  },
  "geo": {
    "dx": {
      "continent_allow": [],
      "continent_deny": [],
      "cq_zone_allow": [],
      "cq_zone_deny": [],
      "itu_zone_allow": [],
      "itu_zone_deny": [],
      "entity_allow": [],
      "entity_deny": [],
      "country_allow": [],
      "country_deny": [],
      "state_allow": [],
      "state_deny": [],
      "grid_allow": { "globs": [], "regexes": [], "case_insensitive": false },
      "grid_deny": { "globs": [], "regexes": [], "case_insensitive": false }
    },
    "spotter": {
      "continent_allow": [],
      "continent_deny": [],
      "cq_zone_allow": [],
      "cq_zone_deny": [],
      "itu_zone_allow": [],
      "itu_zone_deny": [],
      "entity_allow": [],
      "entity_deny": [],
      "country_allow": [],
      "country_deny": [],
      "state_allow": [],
      "state_deny": [],
      "grid_allow": { "globs": [], "regexes": [], "case_insensitive": false },
      "grid_deny": { "globs": [], "regexes": [], "case_insensitive": false }
    },
    "require_resolvers": false
  },
  "enrichment": {
    "lotw": "Any",
    "in_master_db": "Any",
    "in_callbook": "Any",
    "membership": []
  }
}
```

You can dump the default filter configuration as JSON from the CLI:

```sh
cargo run --features cli -- --dump-default-filter > filter.json
```
