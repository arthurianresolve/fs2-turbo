# Historical performance validation record

> Historical record only. This focused run predates the schema-version-2
> Rust-native measurement protocol and is not fs2 1.0 release evidence.

## Commit checked
- Current head: `e2b74d8`
- Branch: `v0.7`
- Change set: unix stats counter multiplication + space conversion optimization

## Validation steps
- `cargo test` passed fully.
- Focused benching run with `sample-size=80`, `measurement-time=1.0`, `warm-up-time=0.2`.
- Additional control runs used `--baseline base` where available.

## Key benchmark outcomes

- `allocated_size`
  - `p = 0.82` vs baseline `base`
  - `No change in performance detected.`

- `duplicate`
  - `p = 0.92` vs baseline `base`
  - `No change in performance detected.`

- `file_allocate_already_satisfied`
  - control run with `--baseline base`: improved
    - `p = 0.00`, `Performance has improved.`
    - one run: `[−14.523% −11.464% −8.3093%]`

- `stats_snapshot/one_snapshot`
  - improved vs baseline
    - `p = 0.01`, `Performance has improved.`
    - one run: `[−8.3288% −4.8338% −1.3780%]`

- `stats_snapshot/four_convenience_queries`
  - `p = 0.08`
  - `No change in performance detected.`

- `windows_root_stats/one_top_level_snapshot`
  - `p = 0.80`
  - `No change in performance detected.`

## Interpretation
No actionable regression was confirmed under baseline-comparison methodology; observed regressions in early comparisons were due to unstable self-baseline handling and sample noise.
