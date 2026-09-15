# Teamup API notes for the weekly importer

Verified against Teamup's public API documentation on 2026-09-15. The
authoritative machine-readable source is Teamup's [current public OpenAPI
export](https://stoplight.io/api/v1/projects/teamup/api/nodes/reference/generated_docs_public.yaml).
The [official Teamup Postman collection](https://www.postman.com/teamup-calendar/teamup-calendar-public-workspace/documentation/9dxuvwb/teamup-calendar-api-examples)
provides current request and response examples.

## Authentication and least privilege

- Every request needs the application API key in the `Teamup-Token` header. A
  calendar key or calendar ID is also part of nearly every endpoint path. A
  calendar key carries its own calendar permissions; use a dedicated read-only
  key for this importer. A bare calendar ID normally needs authenticated-user
  access. Teamup documents `GET /check-access` for validating only the
  application API key; it does not prove access to a target calendar or its
  comments. ([Authentication](https://apidocs.teamup.com/docs/api/e60b71a05cf57-authentication),
  [OpenAPI security schemes](https://stoplight.io/api/v1/projects/teamup/api/nodes/reference/generated_docs_public.yaml))
- `Authorization: Bearer <auth_token>` is only needed when the calendar key does
  not grant the required access. The token endpoint accepts an account password
  and optional 2FA code, so this importer should avoid that path when a
  least-privilege calendar key suffices. A password-protected calendar instead
  uses `Teamup-Password`. ([Authentication](https://apidocs.teamup.com/docs/api/e60b71a05cf57-authentication),
  [obtaining a bearer token](https://apidocs.teamup.com/docs/api/80b2ef0762cbe-obtaining-a-bearer-token))
- Send credentials only to `https://api.teamup.com`; do not log headers or the
  calendar key.

## Weekly event fetch

Use:

```http
GET /{calendarKeyOrId}/events?startDate=YYYY-MM-DD&endDate=YYYY-MM-DD&tz=Europe%2FCopenhagen
Teamup-Token: <application-api-key>
```

- `startDate` defaults to today. `endDate` defaults to `startDate` and is
  inclusive. `tz` defines the date-range timezone and the timezone in which
  event times are returned; otherwise Teamup uses the calendar's timezone.
  Pass `Europe/Copenhagen` explicitly for the importer. ([official Postman Get
  events example](https://www.postman.com/teamup-calendar/teamup-calendar-public-workspace/api/56c05d8a-3a2d-4630-995c-ae1ec4543e29/documentation/29733264-77b6e3f8-e3f7-4e05-8fc4-76da9cce90aa?collection=29733264-77b6e3f8-e3f7-4e05-8fc4-76da9cce90aa&version=f841e66f-5c8a-44f5-b44f-b688e06e85b9),
  [Get events](https://apidocs.teamup.com/docs/api/0f9f896800ffe-get-events))
- The normal date-range endpoint has no `limit`, `offset`, cursor, or next-page
  field in its current contract; its response is `{ "events": [...],
  "timestamp": ... }`. Teamup's separate keyword-search form of the endpoint
  does support `limit` and `offset`, but the importer does not need search.
  ([OpenAPI](https://stoplight.io/api/v1/projects/teamup/api/nodes/reference/generated_docs_public.yaml))
- The current documentation does **not** say precisely whether a non-recurring
  event that starts before the range but overlaps its first day is included.
  It does show an event extending beyond the range's end. The first-day boundary
  must be checked with the real calendar. Until verified, query enough time
  before the week to cover the longest supported shift (at least one day for
  ordinary overnight shifts), optionally one day after it, then filter locally
  against the half-open Copenhagen week interval so boundary-crossing shifts
  are not missed.
- Teamup documents date/time values as ISO 8601. Timed-event examples include a
  UTC offset in `start_dt` and `end_dt`; an all-day example uses local date-time
  strings without an offset. Parse offsets when present, and interpret unoffset
  values in the explicit request/calendar timezone. The event's `tz` field is
  the timezone used to evaluate a recurrence and may be `null` for a
  non-recurring event; it is not a substitute for parsing the returned
  timestamps. ([Authentication: date and time formats](https://apidocs.teamup.com/docs/api/e60b71a05cf57-authentication),
  [official Postman examples](https://www.postman.com/teamup-calendar/teamup-calendar-public-workspace/api/56c05d8a-3a2d-4630-995c-ae1ec4543e29/documentation/29733264-77b6e3f8-e3f7-4e05-8fc4-76da9cce90aa?collection=29733264-77b6e3f8-e3f7-4e05-8fc4-76da9cce90aa&version=f841e66f-5c8a-44f5-b44f-b688e06e85b9))

## Recurring occurrences and exceptions

- A date-range fetch expands an RRULE and returns every occurrence in the
  requested range, accounting for existing exceptions. A recurring occurrence ID is
  documented as `<master-event-id>-rid-<unix-start-timestamp>`, for example
  `123-rid-1712245438`. `series_id` identifies the master event. Do not generate
  or normalize this ID; store the exact value returned by Teamup. ([OpenAPI
  `Event.read`](https://stoplight.io/api/v1/projects/teamup/api/nodes/reference/generated_docs_public.yaml))
- `ristart_dt` is the recurrence instance's original start and is always UTC.
  Teamup preserves it when an exception is moved because the edited start no
  longer identifies the original recurrence slot. Persist `id`, `series_id`,
  and `ristart_dt` together so moved exceptions can be reconciled. Also persist
  the current `start_dt`, `end_dt`, and `version`. ([OpenAPI
  `Event.read`](https://stoplight.io/api/v1/projects/teamup/api/nodes/reference/generated_docs_public.yaml))
- Actual-access validation still needs to confirm whether the occurrence `id`
  itself remains unchanged after moving one occurrence. The importer must not
  assume that property from the ID's textual format.

## Comments are not event notes

- Teamup models `notes` as the event description. Comments are separate:
  `comments_enabled`, `comments_visibility`, and a `comments` array. Import SPS
  text from comments only; never fall back to `notes`. ([OpenAPI event
  schemas](https://stoplight.io/api/v1/projects/teamup/api/nodes/reference/generated_docs_public.yaml),
  [Teamup event fields](https://calendar.teamup.com/kb/what-are-event-fields/))
- When comments are enabled, `comments_visibility` is either `all_users` or
  `users_with_modify_permission`. Teamup says read-only users can add and view
  comments when the event permits all users; modify-only comments are hidden
  from read-only access, and a read-only-no-details key hides event details.
  The setting can be overridden per event. Therefore a read-only calendar key
  is sufficient only when the relevant events expose comments to all users.
  ([Teamup event comments](https://calendar.teamup.com/kb/event-comments/),
  [OpenAPI](https://stoplight.io/api/v1/projects/teamup/api/nodes/reference/generated_docs_public.yaml))
- The API has create/update/delete comment operations but no standalone list
  operation. The single-event response is documented to include auxiliary
  information, including comments; the older
  `/{calendarKeyOrId}/events/{eventId}/aux` endpoint is explicitly deprecated.
  Fetch the single event by the returned occurrence ID when comments are not
  reliably present in the date-range response. ([OpenAPI event and auxiliary
  endpoints](https://stoplight.io/api/v1/projects/teamup/api/nodes/reference/generated_docs_public.yaml))
- Each comment has its own integer `id`, occurrence-aware `event_id`, `message`,
  `creation_dt`, optional `update_dt`, and optional `remote_id`. Depending on the
  requested format, `message` can be a string or an object containing
  `markdown` and `html`. Persist the comment ID and normalized message snapshot,
  not only its timestamp. ([OpenAPI `EventComment`](https://stoplight.io/api/v1/projects/teamup/api/nodes/reference/generated_docs_public.yaml))
- An absent `comments` value is not enough to conclude that no comments exist:
  it may reflect comments being disabled, hidden by the key's permission, or a
  response-shape difference. Inspect `comments_enabled` and
  `comments_visibility`, then report inaccessible comments as a configuration
  error rather than as an empty SPS record.

## Changed-event behavior

`GET /{calendarKeyOrId}/events?modifiedSince=<unix-timestamp>` is useful as an
optional optimization, not as the source of truth for a manual weekly run:

- The response returns events created, updated, or deleted after the supplied
  timestamp and includes a server `timestamp`; Teamup recommends sending that
  returned timestamp on the next request.
- Deleted events have non-null `delete_dt`.
- Changes cannot be restricted to a calendar date range and are retained for a
  maximum of 30 days.
- Recurring changes return only the master event and RRULE, not expanded
  occurrences. For a master, `end_dt` is the end of the whole series and
  `duration` is one occurrence's duration.
- `mode=synchronize` is the default. It omits events both created and deleted
  inside the polling interval; the prose says `mode=monitor` includes them.
  `mode` is missing from the declared OpenAPI parameters and the prose contains
  a spelling error elsewhere, so verify server acceptance before relying on it.

These are all explicit in Teamup's [Get changed events
contract](https://apidocs.teamup.com/docs/api/8266ae959a7f8-get-changed-events).

The changed-events contract does not say that adding, editing, or removing a
comment updates the enclosing event. The same OpenAPI specification models
`event_comment.created`, `event_comment.modified`, and
`event_comment.removed` as activity types separate from `event.modified`.
Consequently, it is unsafe to use `modifiedSince` to decide whether comments
need re-reading. Every weekly run must fetch the selected date range and re-read
its comments, then compare comment IDs/snapshots with local state.

## Checks requiring Jonas's real calendar access

### Access results from 2026-09-15

- The configured application API key and calendar key successfully read the
  2026-09-14 through 2026-09-20 week: 7 events, including one expanded recurring
  occurrence.
- All seven events exposed `comments_enabled`, `comments_visibility` was
  `all_users`, and each detail response contained a comments list. The current
  week contained no comments.
- Calendar configuration reports `Europe/Copenhagen` with dynamic timezone
  handling enabled.
- A count-only scan of the available 2025-01-01 through 2026-09-20 history
  returned 363 events and one ordinary historical comment. This proves actual
  comment access, but that comment was not marked `uni`; no stored real SPS
  example was available to exercise the parser.
- Jonas subsequently supplied all seven helper subcalendar mappings. These
  were resolved against live configuration and stored by subcalendar ID in
  ignored local configuration. Source identity now uses subcalendar membership;
  destination identity validation remains pending.
- No data was written, and event titles, helper values, and comment text were
  not printed during these checks.

The following checks still remain before apply mode:

1. Confirm with Jonas that the calendar key exposes only the intended
   sub-calendars.
2. Verify the first-day overlap case with a known overnight/multi-day event.
3. For an event containing a real `uni ...` comment, inspect both the range
   response and `GET /events/{eventId}`. Confirm the comment body and ID are
   visible with the chosen read-only key and determine whether the body is HTML,
   Markdown, or both.
4. Check events with comments disabled and with modify-only visibility so the
   importer can distinguish no comment from inaccessible comment in actual API
   responses.
5. Inspect an ordinary recurring occurrence and a moved or otherwise edited
   single occurrence. Confirm returned `id`, `series_id`, `ristart_dt`, and
   comment `event_id`; also check whether comments attached to a series are
   inherited by its occurrences or change after a future-series split.
6. If `modifiedSince` is ever enabled, add/edit/delete a harmless test comment
   and verify whether that action appears there. Regardless of the result,
   retain the full weekly comment refresh because the documented contract does
   not guarantee it.
