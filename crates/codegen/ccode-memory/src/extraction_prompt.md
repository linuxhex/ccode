Analyze the following conversation messages and extract key knowledge items.

For each item, provide:
1. **kind**: One of: Decision, Constraint, ExcludedApproach, UserPreference, Correction, ArchitecturalDecision, PerformanceFinding, SecurityConstraint, ApiConvention, Dependency
2. **content**: A concise summary of the knowledge (not the raw message)
3. **confidence**: Your confidence level 0.0-1.0
4. **tags**: Relevant tags for categorization

Rules:
- Extract only factual knowledge, not questions or unclear statements
- Summarize the content concisely (not raw quote)
- Be conservative: when in doubt, don't extract
- Each item must be independently understandable without context
- Use the original language (Chinese or English) for content

Output format (JSON array):
```json
[
  {"kind": "Constraint", "content": "...", "confidence": 0.9, "tags": ["rust", "error-handling"]},
  ...
]
```

Messages:
