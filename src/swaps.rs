use anyhow::{bail, Result};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SwapIntent {
    pub amount: String,
    pub from_asset: String,
    pub to_asset: String,
}

impl SwapIntent {
    pub fn parse(input: &str) -> Result<Self> {
        let parts = input.split_whitespace().collect::<Vec<_>>();
        if parts.len() != 4 || !parts[2].eq_ignore_ascii_case("to") {
            bail!("expected: /swap plan <amount> <from_asset> to <to_asset>");
        }

        let amount = parts[0].trim();
        let from_asset = parts[1].trim();
        let to_asset = parts[3].trim();

        if amount.is_empty() || from_asset.is_empty() || to_asset.is_empty() {
            bail!("swap amount and assets are required");
        }

        Ok(Self {
            amount: amount.to_string(),
            from_asset: from_asset.to_string(),
            to_asset: to_asset.to_string(),
        })
    }

    pub fn mira_handoff_prompt(&self) -> String {
        format!(
            "Quote, explain, and risk-check this Omniston v1beta8 route: {} {} to {}. Use RFQ first, do not execute without explicit wallet confirmation, preserve quote IDs and settlement parameters, and track the submitted trade by quote ID, trader address, and outgoing transaction hash.",
            self.amount, self.from_asset, self.to_asset
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_swap_plan() {
        assert_eq!(
            SwapIntent::parse("10 TON to USDT").unwrap(),
            SwapIntent {
                amount: "10".to_string(),
                from_asset: "TON".to_string(),
                to_asset: "USDT".to_string(),
            }
        );
    }

    #[test]
    fn rejects_ambiguous_swap_plan() {
        assert!(SwapIntent::parse("10 TON USDT").is_err());
    }

    #[test]
    fn builds_mira_handoff_prompt() {
        let prompt = SwapIntent::parse("10 TON to USDT")
            .unwrap()
            .mira_handoff_prompt();
        assert!(prompt.contains("Omniston v1beta8"));
        assert!(prompt.contains("10 TON to USDT"));
        assert!(prompt.contains("do not execute"));
    }
}
