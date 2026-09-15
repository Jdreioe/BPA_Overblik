from datetime import datetime

from .models import Outcome, PlanItem


def is_reminder(title: str) -> bool:
    """Jonas's shared check-the-roster reminders are not helper shifts."""
    title = title.strip().casefold()
    prefix = "husk at checke"
    return title == prefix or title.startswith((prefix + " ", prefix + "..."))


def meeting_item(
    title: str, key: str, start: datetime, end: datetime
) -> PlanItem | None:
    """Title-based meeting category; never implies SPS or DUOS hours."""
    if title.strip().casefold() != "p-møde":
        return None
    return PlanItem(
        key,
        "mithf",
        "mithf.set_meeting",
        Outcome.PENDING_INTEGRATION,
        "P-MØDE → Vagtmøde for the full event; category read-back pending",
        {
            "category": "Vagtmøde",
            "category_code": "4:1",
            "starts_at": start.isoformat(),
            "ends_at": end.isoformat(),
        },
    )
