"""Reasoning module - Model calling and context management."""

from ccode.reasoning.sampler import (
    Provider,
    ProviderConfig,
    SampleRequest,
    SampleResponse,
    StreamChunk,
    StreamChannel,
    ThinkingConfig,
    ProviderRouter,
    OpenAIProvider,
    ClaudeProvider,
    GeminiProvider,
)
from ccode.reasoning.context import ContextManager
from ccode.reasoning.engine import ReasoningEngine
from ccode.reasoning.prompt import (
    PromptBuilder,
    StablePrefix,
    ProjectContext,
)
from ccode.reasoning.memory import (
    CompactionPipeline,
    CompactionLevel,
    SummaryValidator,
    TemperatureZone,
    TemperatureManager,
)

__all__ = [
    # Sampler
    "Provider",
    "ProviderConfig",
    "SampleRequest",
    "SampleResponse",
    "StreamChunk",
    "StreamChannel",
    "ThinkingConfig",
    "ProviderRouter",
    "OpenAIProvider",
    "ClaudeProvider",
    "GeminiProvider",
    # Context
    "ContextManager",
    "ReasoningEngine",
    # Prompt
    "PromptBuilder",
    "StablePrefix",
    "ProjectContext",
    # Memory
    "CompactionPipeline",
    "CompactionLevel",
    "SummaryValidator",
    "TemperatureZone",
    "TemperatureManager",
]
