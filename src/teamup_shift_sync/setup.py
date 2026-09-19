"""Resumable, read-only setup. Secrets never enter the setup document."""

from __future__ import annotations

import hashlib
import json
import os
import re
import sys
import tempfile
import uuid
from datetime import date, datetime, timedelta
from pathlib import Path
from urllib.parse import urlsplit
from zoneinfo import ZoneInfo

from .config import load_config
from .destinations import employment_available
from .models import AppConfig, HelperMapping
from .teamup import TeamUpClient


class SetupError(RuntimeError):
    """Danish text safe for the normal setup screen."""


class CredentialStore:
    """Select only OS stores. Never fall back to plaintext or a null backend."""

    def __init__(self):
        try:
            if sys.platform == "win32":
                from keyring.backends.Windows import WinVaultKeyring

                self.backend = WinVaultKeyring()
            elif sys.platform == "darwin":
                from keyring.backends.macOS import Keyring

                self.backend = Keyring()
            else:
                from keyring.backends.SecretService import Keyring

                self.backend = Keyring()
            if self.backend.priority <= 0:
                raise RuntimeError
        except Exception:  # noqa: BLE001 - redact credential and browser exceptions
            raise SetupError(
                "Lås computerens nøglering op, og prøv igen. På Linux kræves Secret Service."
            ) from None

    def get(self, key):
        try:
            value = self.backend.get_password("teamup-shift-sync", key)
            if value is None:
                raise RuntimeError
            return json.loads(value)
        except Exception:  # noqa: BLE001 - redact credential and browser exceptions
            raise SetupError(
                "De gemte TeamUp-oplysninger kunne ikke læses. Tilslut kalenderen igen."
            ) from None

    def put(self, key, value):
        try:
            self.backend.set_password("teamup-shift-sync", key, json.dumps(value))
        except Exception:  # noqa: BLE001 - redact credential and browser exceptions
            raise SetupError(
                "TeamUp-oplysningerne kunne ikke gemmes i computerens nøglering."
            ) from None


def calendar_reference(link: str) -> str:
    parsed = urlsplit(link.strip())
    parts = parsed.path.strip("/").split("/")
    if (
        parsed.scheme != "https"
        or parsed.netloc not in {"teamup.com", "www.teamup.com"}
        or not parts
        or not re.fullmatch(r"ks[a-zA-Z0-9]+", parts[0])
    ):
        raise SetupError(
            "Indsæt et TeamUp-kalenderlink, der starter med https://teamup.com/ks. Brug et delt link med adgang til vagter og kommentarer."
        )
    return parts[0]


def choice(identifier, name):
    if (
        identifier is None
        or not str(identifier)
        or not isinstance(name, str)
        or not name.strip()
    ):
        raise SetupError(
            "Tjenesten returnerede et valg uden nummer eller læsbart navn. Prøv at logge ind igen."
        )
    return {"id": str(identifier), "name": " ".join(name.split())}


def named(raw):
    return choice(
        raw.get("id"),
        next(
            (
                raw[k]
                for k in ("name", "navn", "title", "description", "displayName")
                if raw.get(k)
            ),
            None,
        ),
    )


def unique(rows):
    result = {}
    for row in rows:
        if row["id"] in result and result[row["id"]] != row:
            raise SetupError(
                "Tjenesten returnerede modstridende personoplysninger. Kontrollér kontoen."
            )
        result[row["id"]] = row
    return list(result.values())


def suggested(name, rows):
    matches = [r["id"] for r in rows if r["name"].casefold() == name.casefold()]
    return matches[0] if len(matches) == 1 else ""


def selected(rows, identifier):
    matches = [r for r in rows if r["id"] == identifier]
    if len(matches) != 1:
        raise SetupError(
            "Et valg mangler eller er ikke længere tilgængeligt. Hent mulighederne igen."
        )
    return matches[0]


class Setup:
    def __init__(
        self, data_dir: Path, transport=None, *, vault=None, client_factory=TeamUpClient
    ):
        self.path = data_dir / "setup.json"
        self.transport = transport
        self._vault = vault
        self.client_factory = client_factory
        self.data = {
            "version": 1,
            "stage": "source",
            "credential": "",
            "calendars": [],
            "arrangements": [],
            "types": [],
            "mithf": [],
            "duos": [],
            "mappings": [],
            "arrangement": "",
            "registration_type": "",
            "account": "",
            "notice": "",
        }
        if self.path.exists():
            try:
                saved = json.loads(self.path.read_text(encoding="utf-8"))
                if saved["version"] != 1:
                    raise ValueError
                self.data.update(saved)
            except (ValueError, KeyError):
                raise SetupError(
                    "Den gemte opsætning kunne ikke læses. Gendan setup.json fra din sikkerhedskopi."
                ) from None

    @property
    def vault(self):
        if self._vault is None:
            self._vault = CredentialStore()
        return self._vault

    def save(self):
        self.path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
        if os.name != "nt":
            self.path.parent.chmod(0o700)
        fd, name = tempfile.mkstemp(dir=self.path.parent, prefix=".setup-")
        try:
            with os.fdopen(fd, "w", encoding="utf-8") as stream:
                json.dump(self.data, stream, ensure_ascii=False)
                stream.flush()
                os.fsync(stream.fileno())
            os.replace(name, self.path)
        finally:
            if os.path.exists(name):
                os.unlink(name)

    def view(self, import_path=None):
        today = datetime.now(
            ZoneInfo(self.data.get("timezone", "Europe/Copenhagen"))
        ).date()
        return {
            k: v
            for k, v in self.data.items()
            if k not in {"credential", "catalog", "imported", "account_ids"}
        } | {
            "week_start": (today - timedelta(days=today.weekday())).isoformat(),
            "can_import": bool(import_path and Path(import_path).is_file()),
            "has_credentials": bool(self.data["credential"]),
        }

    def config(self):
        secret = self.vault.get(self.data["credential"])
        mappings = {}
        for row in self.data["mappings"]:
            if row["excluded"]:
                continue
            source = selected(self.data["calendars"], row["source"])
            mit = selected(self.data["mithf"], row["mithf"])
            duos = selected(self.data["duos"], row["duos"])
            mappings[row["source"]] = HelperMapping(
                row["source"],
                source["name"],
                mit["name"],
                duos["name"],
                duos["id"],
                mit["id"],
            )
        return AppConfig(
            timezone=self.data.get("timezone", "Europe/Copenhagen"),
            default_helper_count=1,
            teamup_calendar_key=secret["calendar"],
            teamup_api_key=secret["api_key"],
            teamup_bearer_token=secret.get("bearer", ""),
            teamup_helper_field="subcalendar",
            duos_arrangement_id=self.data["arrangement"],
            duos_registration_type=self.data["registration_type"],
            helpers=mappings,
            teamup_subcalendar_helpers={
                k: m.teamup_display_name for k, m in mappings.items()
            },
            teamup_lookback_days=self.data.get("lookback_days", 7),
            mithf_customer_id=self.data.get("account_ids", {}).get("customer", ""),
            mithf_grant_id=self.data.get("account_ids", {}).get("grant", ""),
        )

    def source_config(self, secret):
        return AppConfig(
            self.data.get("timezone", "Europe/Copenhagen"),
            1,
            secret["calendar"],
            secret["api_key"],
            secret.get("bearer", ""),
            "subcalendar",
            "",
            "",
            {},
        )

    def source_catalog(self, secret):
        client = self.client_factory(self.source_config(secret))
        configuration = client._get("/configuration", {})["configuration"]
        calendars = unique(
            [
                choice(r["id"], r["name"])
                for r in configuration["subcalendars"]
                if r.get("active", True)
            ]
        )
        if not calendars:
            raise SetupError(
                "Kalenderlinket giver ingen aktive kalendere. Kontrollér delingsrettighederne."
            )
        now = datetime.now(ZoneInfo(client.timezone.key))
        from .operations import resolve_week

        week = resolve_week(timezone_name=client.timezone.key, now=now)
        # Every fetched event is hydrated, so unreadable comments block setup.
        events = client.fetch_occurrences(week.from_date, week.to_date)
        if any(
            (e.raw.get("readonly") and e.title.casefold() in {"reserved", "reserveret"})
            or e.raw.get("details_hidden")
            or e.raw.get("permission")
            in {"read_only_without_details", "add_only_without_details"}
            for e in events
        ):
            raise SetupError(
                "Kalenderlinket skjuler vagtdetaljer. Bed kalenderens ejer om læseadgang til detaljer og kommentarer."
            )
        return calendars, (
            "Vagter og kommentarer er kontrolleret for denne uge."
            if events
            else "Ugen er tom. Adgang til hændelseslisten er kontrolleret; kommentarer kontrolleres, når der findes vagter."
        )

    def connect(self, link, api_key):
        secret = {"calendar": calendar_reference(link), "api_key": api_key.strip()}
        if not secret["api_key"]:
            raise SetupError(
                "Indsæt din TeamUp API-nøgle. Du kan få hjælp af appens vedligeholder til at anmode om den."
            )
        # Persist first so a failed network call can be retried after restart.
        credential = uuid.uuid4().hex
        self.vault.put(credential, secret)
        self.data.update(
            credential=credential,
            stage="source",
            mappings=[],
            catalog=None,
            imported={},
        )
        self.save()
        self.refresh_source()

    def refresh_source(self):
        calendars, notice = self.source_catalog(self.vault.get(self.data["credential"]))
        if calendars != self.data["calendars"]:
            self.data.update(mappings=[], catalog=None)
        self.data.update(calendars=calendars, notice=notice, stage="destinations")
        self.save()

    def import_config(self, path):
        config = load_config(Path(path))
        if config.default_helper_count != 1:
            raise SetupError(
                "Opsætningen kræver præcis én hjælper pr. vagt. Tilslut kalenderen igen med hjælperkalendere."
            )
        if config.teamup_helper_field != "subcalendar":
            raise SetupError(
                "Den gamle opsætning bruger ikke hjælperkalendere. Tilslut TeamUp med kalenderlinket for at vælge dem."
            )
        credential = uuid.uuid4().hex
        self.vault.put(
            credential,
            {
                "calendar": config.teamup_calendar_key,
                "api_key": config.teamup_api_key,
                "bearer": config.teamup_bearer_token,
            },
        )
        self.data.update(
            credential=credential,
            stage="source",
            timezone=config.timezone,
            lookback_days=config.teamup_lookback_days,
            arrangement=config.duos_arrangement_id,
            registration_type=config.duos_registration_type,
            mappings=[],
            catalog=None,
            imported={
                k: {
                    "mithf_name": m.mithf_name,
                    "duos": m.duos_employee_number,
                    "duos_name": m.duos_name,
                }
                for k, m in config.helpers.items()
            },
        )
        self.save()
        self.refresh_source()
        self.data["notice"] += (
            " Den gamle opsætning er importeret som forslag. Bekræft alle navne og valg. Originalfilen er ikke ændret og kan stadig indeholde nøgler."
        )
        self.save()

    def call(self, service, action, **payload):
        return self.transport.request(service, action, payload)

    def destination_catalog(self, arrangement=""):
        mithf = unique(
            [
                choice(h["vid"], h["navn"])
                for g in self.call("mithf", "hjaelperliste")["grupper"]
                for h in g["hjaelpere"]
            ]
        )
        options = self.call("mithf", "muligheder")["muligheder"]
        customers = options["kunder"]
        grants = [g for g in options["bevillinger"] if g.get("valgbar")]
        if len(customers) != 1 or len(grants) != 1:
            raise SetupError(
                "MitHF har ingen eller flere kunder/bevillinger. Appen understøtter endnu kun én kunde og én valgbar bevilling. Vælg den rette konto i MitHF, eller kontakt appens vedligeholder."
            )
        customer, grant = named(customers[0]), named(grants[0])
        today = (
            datetime.now(ZoneInfo(self.data.get("timezone", "Europe/Copenhagen")))
            .date()
            .isoformat()
        )
        arrangements = unique(
            [
                choice(
                    p["id"],
                    p["name"] + (f" · {p['suffix']}" if p.get("suffix") else ""),
                )
                for p in self.call("duos", "portfolios", dateOfActivePortfolio=today)
                if p.get("type") == "Ordnings SPS"
            ]
        )
        if not arrangements:
            raise SetupError(
                "DUOS har ingen aktiv SPS-ordning. Kontrollér kontoen og ordningens gyldighed hos DUOS."
            )
        result = {
            "mithf": mithf,
            "arrangements": arrangements,
            "types": [],
            "duos": [],
            "account": f"MitHF: {customer['name']} · {grant['name']}",
            "account_ids": {"customer": customer["id"], "grant": grant["id"]},
        }
        if arrangement:
            selected(arrangements, arrangement)
            result["types"] = unique(
                [named(t) for t in self.call("duos", "types", portfolioId=arrangement)]
            )
            result["duos"] = unique(
                [
                    choice(e["helperId"], e["helperName"])
                    for e in self.call(
                        "duos",
                        "employments",
                        portfolioId=arrangement,
                        dateOfActiveEmployment=today,
                    )
                    if employment_available(e, date.fromisoformat(today))
                ]
            )
            if not result["types"] or not result["duos"]:
                raise SetupError(
                    "Ordningen har ingen tilgængelige registreringstyper eller aktive hjælpere. Kontrollér ansættelserne hos DUOS."
                )
        return result

    def discover(self, arrangement=None):
        catalog = self.destination_catalog()
        target = arrangement if arrangement is not None else self.data["arrangement"]
        if target and not any(r["id"] == target for r in catalog["arrangements"]):
            target = ""
        if not target and len(catalog["arrangements"]) == 1:
            target = catalog["arrangements"][0]["id"]
        if target:
            catalog = self.destination_catalog(target)
        old = self.data.get("catalog")
        self.data.update(catalog)
        self.data.update(
            arrangement=target,
            catalog=catalog,
            stage="helpers" if target else "destinations",
        )
        if not any(t["id"] == self.data["registration_type"] for t in catalog["types"]):
            self.data["registration_type"] = (
                catalog["types"][0]["id"] if len(catalog["types"]) == 1 else ""
            )
        # Preserve edits only while all identities and account choices are unchanged.
        if old != catalog or not self.data["mappings"]:
            rows = []
            for source in self.data["calendars"]:
                imported = self.data.get("imported", {}).get(source["id"], {})
                duos_id = suggested(source["name"], catalog["duos"])
                if imported:
                    candidate = next(
                        (
                            r
                            for r in catalog["duos"]
                            if r["id"] == imported["duos"]
                            and r["name"] == imported["duos_name"]
                        ),
                        None,
                    )
                    duos_id = candidate["id"] if candidate else ""
                rows.append(
                    {
                        "source": source["id"],
                        "mithf": suggested(
                            imported.get("mithf_name", source["name"]), catalog["mithf"]
                        ),
                        "duos": duos_id,
                        "excluded": False,
                    }
                )
            self.data["mappings"] = rows
        self.save()

    def edit(self, params):
        if "registration_type" in params:
            selected(self.data["types"], params["registration_type"])
            self.data["registration_type"] = params["registration_type"]
        else:
            row = next(
                r for r in self.data["mappings"] if r["source"] == params["source"]
            )
            if "excluded" in params:
                if type(params["excluded"]) is not bool:
                    raise SetupError("Ugyldigt kalendervalg.")
                row["excluded"] = params["excluded"]
            for service in ("mithf", "duos"):
                if service in params:
                    selected(self.data[service], params[service])
                    row[service] = params[service]
        self.data["stage"] = "helpers"
        self.save()

    def validate_choices(self):
        if {r["source"] for r in self.data["mappings"]} != {
            c["id"] for c in self.data["calendars"]
        }:
            raise SetupError(
                "Gennemgå alle kalendere, og vælg hjælpere eller udelad dem."
            )
        selected(self.data["arrangements"], self.data["arrangement"])
        selected(self.data["types"], self.data["registration_type"])
        rows = [r for r in self.data["mappings"] if not r["excluded"]]
        if not rows:
            raise SetupError("Vælg mindst én hjælperkalender.")
        for service in ("mithf", "duos"):
            values = []
            for row in rows:
                person = selected(self.data[service], row[service])
                # Existing MitHF readback identifies assignments by name. Duplicate
                # names cannot be reconciled safely even with a chosen stable ID.
                if (
                    service == "mithf"
                    and sum(p["name"] == person["name"] for p in self.data[service])
                    != 1
                ):
                    raise SetupError(
                        "MitHF har flere hjælpere med samme navn. Få navnene gjort entydige i MitHF, før de kan overføres sikkert."
                    )
                values.append(person["id"])
            if len(values) != len(set(values)):
                raise SetupError(
                    "Flere kalendere er valgt til samme hjælper. Ret valgene, eller udelad en kalender."
                )

    def revalidate(self):
        calendars, _notice = self.source_catalog(
            self.vault.get(self.data["credential"])
        )
        fresh = self.destination_catalog(self.data["arrangement"])
        if calendars != self.data["calendars"] or fresh != self.data.get("catalog"):
            self.data.update(
                calendars=calendars,
                stage="destinations",
                catalog=None,
                mappings=[],
                notice="Navne, ansættelser eller kontovalg er ændret. Hent mulighederne og bekræft opsætningen igen.",
            )
            self.save()
            raise SetupError(self.data["notice"])
        self.validate_choices()

    def confirm(self):
        self.revalidate()
        self.data["stage"] = "ready"
        self.save()

    def preview(self, state_path, from_date, to_date):
        if self.data["stage"] != "ready":
            raise SetupError("Bekræft opsætningen først.")
        self.revalidate()
        from .operations import preview_connected

        # Separate state for each complete, confirmed account/mapping combination.
        identity = {
            k: self.data[k]
            for k in ("account_ids", "arrangement", "registration_type", "mappings")
        }
        identity["calendar"] = self.config().teamup_calendar_key
        scope = hashlib.sha256(
            json.dumps(identity, sort_keys=True).encode()
        ).hexdigest()[:24]
        return preview_connected(
            config=self.config(),
            transport=self.transport,
            state_path=Path(state_path).parent / f"sync-{scope}.sqlite3",
            from_date=from_date,
            to_date=to_date,
            now=datetime.now(ZoneInfo(self.config().timezone)),
        )
