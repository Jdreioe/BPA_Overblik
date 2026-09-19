from __future__ import annotations

from datetime import date, datetime, timedelta
from typing import Any
from zoneinfo import ZoneInfo

from .browser import DestinationError, DestinationTransport
from .models import (
    AppConfig,
    DestinationSnapshot,
    DuosRegistration,
    MitHfShift,
    PlanItem,
    TimeInterval,
)
from .teamup import _teamup_datetime


def employment_available(employment: dict[str, Any], day: date) -> bool:
    """Match DUOS's active flag or an employment covering the selected date."""
    if employment.get("isActive") is True:
        return True
    try:
        start = date.fromisoformat(str(employment["startDate"])[:10])
        end = employment.get("endDate")
        return start <= day and (not end or day <= date.fromisoformat(str(end)[:10]))
    except (KeyError, ValueError):
        return False


class Destinations:
    def __init__(self, transport: DestinationTransport, config: AppConfig):
        self.transport = transport
        self.config = config
        self.timezone = ZoneInfo(config.timezone)
        self.helpers: dict[str, str] = {}
        self.creation_options: dict[str, str] = {}

    def call(self, system: str, action: str, **payload: Any) -> Any:
        return self.transport.request(system, action, payload)

    def validate(self, start: datetime) -> None:
        """Resolve configured identities against both services, without guessing."""
        response = self.call("mithf", "hjaelperliste")
        helpers: dict[str, list[str]] = {}
        for group in response["grupper"]:
            for helper in group["hjaelpere"]:
                helpers.setdefault(helper["navn"], []).append(str(helper["vid"]))
        self.helpers = {
            name: ids[0] for name, ids in helpers.items() if len(set(ids)) == 1
        }
        options = self.call("mithf", "muligheder")["muligheder"]
        customers = options["kunder"]
        grants = [g for g in options["bevillinger"] if g.get("valgbar")]
        if len(customers) != 1 or len(grants) != 1:
            raise DestinationError(
                "MitHF needs an explicit customer/grant choice; multiple options exist"
            )
        self.creation_options = {
            "klient": str(customers[0]["id"]),
            "bevilling": str(grants[0]["id"]),
        }
        if (
            self.config.mithf_customer_id
            and self.creation_options["klient"] != self.config.mithf_customer_id
            or self.config.mithf_grant_id
            and self.creation_options["bevilling"] != self.config.mithf_grant_id
        ):
            raise DestinationError("MitHF account changed; confirm setup again")
        portfolios = self.call(
            "duos", "portfolios", dateOfActivePortfolio=start.date().isoformat()
        )
        portfolio = [
            p for p in portfolios if str(p["id"]) == self.config.duos_arrangement_id
        ]
        if len(portfolio) != 1 or portfolio[0].get("type") != "Ordnings SPS":
            raise DestinationError(
                "Configured DUOS arrangement does not resolve to Ordnings SPS"
            )
        types = self.call("duos", "types", portfolioId=self.config.duos_arrangement_id)
        if not any(str(t["id"]) == self.config.duos_registration_type for t in types):
            raise DestinationError("Configured DUOS registration type is unavailable")
        employments = self.call(
            "duos",
            "employments",
            portfolioId=self.config.duos_arrangement_id,
            dateOfActiveEmployment=start.date().isoformat(),
        )
        for mapping in self.config.helpers.values():
            if (
                mapping.mithf_name not in self.helpers
                or mapping.mithf_id
                and self.helpers[mapping.mithf_name] != mapping.mithf_id
            ):
                raise DestinationError(
                    "A configured helper does not uniquely match MitHF"
                )
            if not any(
                employment_available(e, start.date())
                and str(e["helperId"]) == mapping.duos_employee_number
                and " ".join(e["helperName"].split())
                == " ".join(mapping.duos_name.split())
                for e in employments
            ):
                raise DestinationError(
                    "A configured helper does not match DUOS employment records"
                )

    def read(self, start: datetime, end: datetime) -> DestinationSnapshot:
        try:
            return DestinationSnapshot(
                self.read_mithf(start, end), self.read_duos(start, end)
            )
        except (KeyError, TypeError, ValueError):
            raise DestinationError(
                "Destination response is incomplete or malformed; no writes allowed"
            ) from None

    def read_mithf(self, start: datetime, end: datetime) -> tuple[MitHfShift, ...]:
        first = (
            (start - timedelta(days=self.config.teamup_lookback_days))
            .date()
            .isoformat()
        )
        last = end.date().isoformat()
        response = self.call("mithf", "plan", fra=first, til=last, frisk=1)
        if (
            response.get("fra") != first
            or response.get("til") != last
            or not isinstance(response.get("dage"), dict)
        ):
            raise DestinationError("MitHF did not confirm requested range coverage")
        raw_by_id = {}
        for day in response["dage"].values():
            for row in day:
                begins = self.local(row["startFaktisk"], row["start"])
                finishes = self.local(row["slutdato"], row["slut"])
                if finishes <= begins:
                    raise DestinationError("Invalid MitHF shift interval")
                if finishes > start and begins < end:
                    if str(row["id"]) in raw_by_id and raw_by_id[str(row["id"])] != row:
                        raise DestinationError(
                            "MitHF returned inconsistent copies of a shift"
                        )
                    raw_by_id[str(row["id"])] = row
        shifts = []
        # Read details for every relevant shift. List badges contain full-shift hours.
        for id_, row in raw_by_id.items():
            extra = self.call("mithf", "ekstra", eids=id_)["ekstra"][id_]
            sps, meetings, sps_ids, meeting_ids = [], [], [], []
            for record in extra["paa"]:
                if record.get("navn") not in {"SPS timer", "Vagtmøde"}:
                    continue
                interval = TimeInterval(
                    self.local(record["fraDato"], record["fra"]),
                    self.local(record["tilDato"], record["til"]),
                )
                if interval.ends_at <= interval.starts_at:
                    raise DestinationError("Invalid MitHF category interval")
                intervals, ids = (
                    (sps, sps_ids)
                    if record["navn"] == "SPS timer"
                    else (meetings, meeting_ids)
                )
                intervals.append(interval)
                ids.append(str(record["id"]))
            shifts.append(
                MitHfShift(
                    id_,
                    self.local(row["startFaktisk"], row["start"]),
                    self.local(row["slutdato"], row["slut"]),
                    1,
                    row["navn"] if row["daekket"] else None,
                    tuple(sps),
                    tuple(meetings),
                    tuple(sps_ids),
                    tuple(meeting_ids),
                )
            )
        return tuple(shifts)

    def read_duos(self, start: datetime, end: datetime) -> tuple[DuosRegistration, ...]:
        registrations = []
        seen: set[str] = set()
        skip = 0
        while True:
            response = self.call(
                "duos",
                "search",
                skip=skip,
                take=100,
                includeFields=[
                    "id",
                    "portfolioId",
                    "helperId",
                    "dutyTypeId",
                    "startDate",
                    "endDate",
                    "statusId",
                ],
            )
            rows, has_more = response["data"], response["hasMore"]
            if (
                not isinstance(rows, list)
                or type(has_more) is not bool
                or (has_more and not rows)
            ):
                raise DestinationError(
                    "DUOS pagination did not establish complete coverage"
                )
            for row in rows:
                id_ = str(row["id"])
                if id_ in seen:
                    raise DestinationError(
                        "DUOS pagination repeated a record; retry the read"
                    )
                seen.add(id_)
                begins, finishes = (
                    self.timestamp(row["startDate"]),
                    self.timestamp(row["endDate"]),
                )
                if finishes <= begins:
                    raise DestinationError("Invalid DUOS registration interval")
                if finishes > start and begins < end:
                    registrations.append(
                        DuosRegistration(
                            id_,
                            str(row["portfolioId"]),
                            str(row["helperId"]),
                            str(row["dutyTypeId"]),
                            begins,
                            finishes,
                            int(row["statusId"]),
                        )
                    )
            skip += len(rows)
            if not has_more:
                break
            if skip > 100000:
                raise DestinationError(
                    "DUOS read exceeded the supported registration count"
                )
        return tuple(registrations)

    def timestamp(self, value: str) -> datetime:
        return _teamup_datetime(value, self.timezone).astimezone(self.timezone)

    def local(self, day: str, clock: str) -> datetime:
        if "." in day:
            dd, mm, yyyy = map(int, day.split("."))
            day = date(yyyy, mm, dd).isoformat()
        return self.timestamp(f"{day}T{clock}")

    def write(self, item: PlanItem, snapshot: DestinationSnapshot) -> str:
        """Send one planned operation. The caller checkpoints and verifies read-back."""
        payload = item.payload
        if item.system == "duos":
            start, end = (
                self.timestamp(payload["starts_at"]),
                self.timestamp(payload["ends_at"]),
            )
            if (
                end > datetime.now(self.timezone)
                or not 0 < (end.timestamp() - start.timestamp()) / 3600 <= 26
            ):
                raise DestinationError(
                    "DUOS requires a completed interval of at most 26 hours"
                )
            if item.destination_id:
                detail = self.call("duos", "detail", id=item.destination_id)
                if detail.get("status") != "Afventer":
                    raise DestinationError(
                        "DUOS has not confirmed that this registration is editable"
                    )
            body = {
                "portfolioId": int(payload["arrangement_id"]),
                "helperId": int(payload["employee_number"]),
                "typeId": int(payload["registration_type"]),
                "startDate": start.isoformat(),
                "endDate": end.isoformat(),
                "clientTimeSnapshot": {
                    "visibleStartLocal": start.replace(tzinfo=None).isoformat(),
                    "visibleEndLocal": end.replace(tzinfo=None).isoformat(),
                    "visibleDurationMinutes": (end.timestamp() - start.timestamp())
                    / 60,
                    "clientTimeZoneId": self.timezone.key,
                    "clientStartOffsetMinutes": int(
                        start.utcoffset().total_seconds() / 60
                    ),
                    "clientEndOffsetMinutes": int(end.utcoffset().total_seconds() / 60),
                },
            }
            if item.destination_id:
                body["registrationId"] = int(item.destination_id)
            self.call("duos", "register", **body)
            # The registration endpoint's response need not contain an ID.
            # Reconciliation finds the unique persisted interval after the write.
            return item.destination_id or ""
        if item.step_key == "mithf.create_shift":
            if payload["helper_count"] != 1:
                raise DestinationError(
                    "Automatic MitHF creation supports exactly one helper per source shift"
                )
            start, end = (
                self.timestamp(payload["starts_at"]),
                self.timestamp(payload["ends_at"]),
            )
            fields = {
                "dato": start.date().isoformat(),
                "start": start.strftime("%H:%M"),
                "slut": end.strftime("%H:%M"),
                "slutdato": end.date().isoformat(),
            }
            if item.destination_id:
                existing = next(
                    s for s in snapshot.mithf_shifts if s.id == item.destination_id
                )
                response = self.call(
                    "mithf",
                    "rettid",
                    eid=item.destination_id,
                    gammeldato=existing.starts_at.date().isoformat(),
                    **fields,
                )
            else:
                response = self.call(
                    "mithf", "opret", antal=1, **self.creation_options, **fields
                )
            id_ = response.get("eid") or item.destination_id
            if not id_ or len(response.get("eids", [id_])) != 1:
                raise DestinationError(
                    "MitHF did not return one created shift identity"
                )
            return str(id_)
        existing = next(
            (s for s in snapshot.mithf_shifts if s.id == item.destination_id), None
        )
        if existing is None:
            raise DestinationError(
                "MitHF parent shift must be read back before assignment or categories"
            )
        if item.step_key == "mithf.assign_helper":
            response = self.call(
                "mithf",
                "book",
                eid=existing.id,
                vid=self.helpers[payload["helper_name"]],
                dato=existing.starts_at.date().isoformat(),
                start=existing.starts_at.strftime("%H:%M"),
            )
            return str(response.get("eid") or existing.id)
        if item.step_key in {"mithf.set_sps", "mithf.set_meeting"}:
            intervals = payload["intervals"]
            if len(intervals) != 1:
                raise DestinationError(
                    "Multiple MitHF SPS writes remain blocked pending live verification"
                )
            meeting = item.step_key == "mithf.set_meeting"
            ids = existing.meeting_record_ids if meeting else existing.sps_record_ids
            if len(ids) > 1:
                raise DestinationError(
                    "Refusing to collapse existing category intervals"
                )
            interval = intervals[0]
            start, end = (
                self.timestamp(interval["starts_at"]),
                self.timestamp(interval["ends_at"]),
            )
            fields = {
                "eid": existing.id,
                "type": "4",
                "dato": existing.starts_at.date().isoformat(),
                "fradato": start.date().isoformat(),
                "tildato": end.date().isoformat(),
                "start": start.strftime("%H:%M"),
                "slut": end.strftime("%H:%M"),
            }
            if ids:
                self.call("mithf", "retreg", rid=ids[0], **fields)
            else:
                self.call(
                    "mithf", "tilfoejreg", typeid="1" if meeting else "5", **fields
                )
            return existing.id
        raise DestinationError("Unsupported sync operation")
