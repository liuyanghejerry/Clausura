//! USD cost caps.
//!
//! Token caps (`max_total_tokens`) bound how many tokens a run bills, but not
//! what it costs: the same token count is a few cents on one model and
//! dollars on another, and a sharded audit multiplies that by the shard
//! count. These helpers price a run's provider-reported usage with the
//! `llm-budget` price table so `max_cost_usd` (per run or per shard) and
//! `sharding.max_total_cost_usd` (the whole run) cap dollars directly.
//!
//! Pricing fails closed: when a cap is set and the model has no known price,
//! the run is refused instead of running without a cap.

use crate::types::{ProviderError, TaskContract, Usage};
use llm_budget::PriceTable;

/// USD cost of `usage` on `model`, or `None` when the model has no price.
pub fn usage_cost_usd(model: &str, usage: &Usage) -> Option<f64> {
    PriceTable::builtin()
        .cost(model, usage.input_tokens, usage.output_tokens)
        .ok()
}

/// Refuse a run whose `max_cost_usd` cannot be enforced because the model
/// has no price.
pub fn ensure_priced(contract: &TaskContract) -> Result<(), ProviderError> {
    if contract.max_cost_usd.is_some()
        && usage_cost_usd(&contract.model, &Usage::default()).is_none()
    {
        return Err(ProviderError::BadRequest(format!(
            "max_cost_usd is set but model '{}' has no known price; \
             refusing to run without an enforceable cost cap",
            contract.model
        )));
    }
    Ok(())
}

/// True once the run's cumulative usage has reached `max_cost_usd`. An
/// unpriced model counts as reached (fail closed).
pub fn cost_cap_reached(contract: &TaskContract, usage: &Usage) -> bool {
    match contract.max_cost_usd {
        Some(cap) => usage_cost_usd(&contract.model, usage).map_or(true, |usd| usd >= cap),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usage(input: u64, output: u64) -> Usage {
        Usage {
            input_tokens: input,
            output_tokens: output,
            total_tokens: input + output,
        }
    }

    #[test]
    fn prices_known_model() {
        let usd = usage_cost_usd("gpt-4o", &usage(1_000_000, 0)).expect("gpt-4o is priced");
        assert!(usd > 0.0);
        // Output tokens cost more than input tokens on gpt-4o.
        let out = usage_cost_usd("gpt-4o", &usage(0, 1_000_000)).unwrap();
        assert!(out > usd);
    }

    fn contract(model: &str, cap: f64) -> TaskContract {
        serde_json::from_value(serde_json::json!({
            "id": "t",
            "name": "t",
            "model": model,
            "prompt_template": "",
            "token_budget": 100000,
            "timeout_secs": 60,
            "max_cost_usd": cap,
        }))
        .expect("minimal contract parses")
    }

    #[test]
    fn cap_compares_dollars_not_tokens() {
        // One token count, one dollar cap: under it on a cheap model, over
        // it on an expensive one.
        let used = usage(500_000, 50_000);
        let cheap = usage_cost_usd("gpt-4o-mini", &used).expect("gpt-4o-mini is priced");
        let pricey = usage_cost_usd("gpt-4o", &used).expect("gpt-4o is priced");
        assert!(cheap < pricey);
        let cap = (cheap + pricey) / 2.0;

        assert!(!cost_cap_reached(&contract("gpt-4o-mini", cap), &used));
        assert!(cost_cap_reached(&contract("gpt-4o", cap), &used));
    }

    #[test]
    fn unpriced_model_fails_closed() {
        let c = contract("no-such-model-xyz", 1.0);
        assert!(ensure_priced(&c).is_err());
        assert!(cost_cap_reached(&c, &Usage::default()));
    }
}
