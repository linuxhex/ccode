
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AgentOperatingMode {
    #[default]
    Standard,
    PlanThenCode,
    Minimal,
    Creative,
}

impl AgentOperatingMode {
    pub const ALL: [Self; 4] = [Self::Standard, Self::PlanThenCode, Self::Minimal, Self::Creative];

    pub fn label(&self) -> &'static str {
        match self {
            Self::Standard => "Standard",
            Self::PlanThenCode => "Plan-then-Code",
            Self::Minimal => "Minimal",
            Self::Creative => "Creative",
        }
    }

    pub fn slug(&self) -> &'static str {
        match self {
            Self::Standard => "standard",
            Self::PlanThenCode => "ptc",
            Self::Minimal => "minimal",
            Self::Creative => "creative",
        }
    }

    pub fn from_slug(s: &str) -> Option<Self> {
        match s.to_lowercase().as_str() {
            "standard" | "default" => Some(Self::Standard),
            "ptc" | "plan-then-code" | "plancode" => Some(Self::PlanThenCode),
            "minimal" | "min" => Some(Self::Minimal),
            "creative" | "explore" => Some(Self::Creative),
            _ => None,
        }
    }

    pub fn system_prompt_addition(&self) -> &'static str {
        match self {
            Self::Standard => "",
            Self::PlanThenCode => {
                "\n\n## Plan-then-Code Mode\n\
                 Before making any code changes, you MUST first produce a clear,\n\
                 structured plan. The plan should include:\n\
                 1. What you intend to change and why\n\
                 2. The specific files and locations affected\n\
                 3. The order of operations\n\
                 Only after the plan is laid out should you proceed with implementation.\n\
                 Prefer precision over exploration (temperature: 0.3)."
            }
            Self::Minimal => {
                "\n\n## Minimal Mode\n\
                 Be concise. Provide direct answers without extensive explanation\n\
                 unless asked. Skip pleasantries and unnecessary context. Focus on\n\
                 the essential information the user needs.\n\
                 Keep responses short and factual (temperature: 0.2)."
            }
            Self::Creative => {
                "\n\n## Creative Mode\n\
                 Explore multiple approaches before settling on a solution. Consider\n\
                 unconventional patterns, alternative architectures, and creative\n\
                 trade-offs. Present options when they exist. Favor innovation over\n\
                 convention when the situation allows.\n\
                 Embrace diverse ideas and unexpected connections (temperature: 0.9)."
            }
        }
    }

    pub fn temperature_hint(&self) -> Option<f32> {
        match self {
            Self::Standard => None,
            Self::PlanThenCode => Some(0.3),
            Self::Minimal => Some(0.2),
            Self::Creative => Some(0.9),
        }
    }

    pub fn auto_expand_tools(&self) -> bool {
        match self {
            Self::Minimal => false,
            _ => true,
        }
    }

    pub fn verbose_explanations(&self) -> bool {
        match self {
            Self::Minimal => false,
            Self::Creative => true,
            _ => true,
        }
    }
}

impl std::fmt::Display for AgentOperatingMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}
