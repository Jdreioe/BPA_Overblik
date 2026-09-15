from __future__ import annotations

from collections import Counter

from .models import SyncPlan


def render_plan(plan: SyncPlan) -> str:
    lines = [
        "DRY RUN — no TeamUp, MitHF, or DUOS writes will be made",
        f"Range: {plan.starts_at.isoformat()} to {plan.ends_at.isoformat()} (end exclusive)",
        f"Evaluated at: {plan.generated_at.isoformat()}",
        "",
    ]
    current_source: str | None = None
    for item in plan.items:
        if item.source_key != current_source:
            current_source = item.source_key
            lines.extend((current_source, "-" * min(100, len(current_source))))
        suffix = f" [destination: {item.destination_id}]" if item.destination_id else ""
        lines.append(
            f"  {item.outcome.value:24} {item.step_key:36} {item.summary}{suffix}"
        )
    if not plan.items:
        lines.append("No source shifts overlap this range.")

    counts = Counter(item.outcome.value for item in plan.items)
    lines.extend(("", "Summary"))
    for outcome, count in sorted(counts.items()):
        lines.append(f"  {outcome:24} {count}")
    return "\n".join(lines)
