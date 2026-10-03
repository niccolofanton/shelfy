//! The cost and time estimate of an analyze request (plan §2.15, §2.9
//! `analyze`; P3-13): input and output tokens, a price for a priced BYOK
//! route, and — for the operator node — an ETA from its measured pace.
//!
//! The token figures are an estimate, not a meter: a per-post model (a fixed
//! prompt, an average number of frames, the catalog's `max_tokens` cap)
//! multiplied by the posts to analyze. The server records real usage
//! separately (P3-09).

use super::prompts::{self, Task};

/// Prompt tokens a catalog call spends before its frames: the system prompt,
/// the user scaffolding and a typical caption and vocabulary hint.
pub const INPUT_BASE_TOKENS: u64 = 750;
/// Prompt tokens a single catalog frame costs the vision model, at the
/// resolution P3-13 sends (≈ 1024 px): an order-of-magnitude figure.
pub const INPUT_FRAME_TOKENS: u64 = 900;
/// Frames an average post sends (a cover and a few slides or keyframes), for
/// the aggregate estimate.
pub const AVERAGE_FRAMES: u64 = 2;
/// The operator node's pace when none has been measured yet: qwen generates
/// about 5 tok/s, so a 768-token catalog answer plus its prompt and a model
/// swap is about three minutes a post (P3-01, L19).
pub const DEFAULT_MS_PER_POST: u64 = 180_000;

/// The estimated tokens and time of an analyze request.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Estimate {
    /// Posts to analyze.
    pub posts: u64,
    /// Estimated prompt tokens over all of them.
    pub input_tokens: u64,
    /// Estimated answer tokens over all of them.
    pub output_tokens: u64,
    /// Estimated wall-clock time, ms, for a one-at-a-time provider (the
    /// operator node); `None` when no pace is known and none was given.
    pub eta_ms: Option<u64>,
}

impl Estimate {
    /// The estimate for analyzing `posts`, with the operator's measured pace
    /// (ms per post) when known.
    #[must_use]
    pub fn of(posts: u64, ms_per_post: Option<u64>) -> Self {
        let per_post_input = INPUT_BASE_TOKENS + AVERAGE_FRAMES * INPUT_FRAME_TOKENS;
        let per_post_output = u64::from(prompts::max_tokens(Task::Catalog, 0));
        let pace = ms_per_post.unwrap_or(DEFAULT_MS_PER_POST);
        Self {
            posts,
            input_tokens: posts.saturating_mul(per_post_input),
            output_tokens: posts.saturating_mul(per_post_output),
            eta_ms: (posts > 0).then(|| posts.saturating_mul(pace)),
        }
    }

    /// The price in US dollars for a priced BYOK route, from its per-million
    /// input and output token prices; `None` for the operator node (its node
    /// is the owner's own, so there is no per-token price).
    #[must_use]
    pub fn cost_usd(&self, input_per_mtok: Option<f64>, output_per_mtok: Option<f64>) -> Option<f64> {
        let (input, output) = (input_per_mtok?, output_per_mtok?);
        let million = 1_000_000.0;
        Some((self.input_tokens as f64 / million) * input + (self.output_tokens as f64 / million) * output)
    }
}

/// A rolling mean of recent catalog-call durations, for the operator ETA.
/// Cheap and `Copy`; the drain feeds it finished calls and the estimate and
/// the queue read it (plan §2.9: "an ETA from recent durations").
#[derive(Clone, Copy, Debug, Default)]
pub struct Pace {
    total_ms: u64,
    samples: u64,
}

impl Pace {
    /// How many recent samples the mean keeps.
    pub const WINDOW: u64 = 20;

    /// An empty pace.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Records one finished catalog call of `ms`, keeping a mean over about
    /// [`Pace::WINDOW`] samples.
    pub fn record(&mut self, ms: u64) {
        if self.samples >= Self::WINDOW {
            // Drop one average sample so the mean follows recent calls.
            let mean = self.total_ms / self.samples;
            self.total_ms -= mean;
            self.samples -= 1;
        }
        self.total_ms = self.total_ms.saturating_add(ms);
        self.samples += 1;
    }

    /// The mean ms per post, or `None` when nothing has been measured.
    #[must_use]
    pub fn ms_per_post(&self) -> Option<u64> {
        (self.samples > 0).then(|| self.total_ms / self.samples)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_estimate_scales_with_the_posts() {
        let zero = Estimate::of(0, None);
        assert_eq!(zero.input_tokens, 0);
        assert_eq!(zero.output_tokens, 0);
        assert_eq!(zero.eta_ms, None);
        let one = Estimate::of(1, None);
        assert_eq!(one.output_tokens, u64::from(prompts::max_tokens(Task::Catalog, 0)));
        assert_eq!(one.eta_ms, Some(DEFAULT_MS_PER_POST));
        let hundred = Estimate::of(100, Some(120_000));
        assert_eq!(hundred.eta_ms, Some(12_000_000));
        assert_eq!(hundred.input_tokens, 100 * one.input_tokens);
    }

    #[test]
    fn a_price_needs_both_rates() {
        let e = Estimate::of(1000, None);
        assert_eq!(e.cost_usd(None, Some(1.0)), None);
        let cost = e.cost_usd(Some(0.5), Some(1.5)).unwrap();
        let expected = (e.input_tokens as f64 / 1e6) * 0.5 + (e.output_tokens as f64 / 1e6) * 1.5;
        assert!((cost - expected).abs() < 1e-9);
    }

    #[test]
    fn the_pace_is_a_rolling_mean() {
        let mut pace = Pace::new();
        assert_eq!(pace.ms_per_post(), None);
        pace.record(100_000);
        pace.record(200_000);
        assert_eq!(pace.ms_per_post(), Some(150_000));
        for _ in 0..40 {
            pace.record(60_000);
        }
        // After many fast calls the mean has followed them down.
        assert!(pace.ms_per_post().unwrap() < 70_000);
    }
}
