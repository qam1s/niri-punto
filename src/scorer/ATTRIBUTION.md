# Bigram profile attribution

The log-probability tables in `tables.rs` are derived from third-party
word-frequency lists:

- Author: Hermit Dave
- Source: <https://github.com/hermitdave/FrequencyWords>
- Inputs: `en_50k.txt` / `ru_50k.txt` (OpenSubtitles-derived word lists)
- Upstream license for content: CC-BY-SA 4.0 (upstream code is MIT;
  upstream README: "MIT License for code. CC-by-sa-4.0 for content")

Derivation (see `gen_profiles.py` next to this file): each word with
frequency *f* contributes its boundary-aware bigrams
(`^,c1`, `c1,c2`, …, `cn,^`) weighted by *f*; the tables store add-k
(*k* = 0.5) smoothed log *P(b|a)*. Cross-word pairs are approximated
through the shared boundary symbol.

CC-BY-SA 4.0 is one-way compatible with GPL-3.0, so these derived
tables ship under this repo's GPL-3.0-or-later license.
