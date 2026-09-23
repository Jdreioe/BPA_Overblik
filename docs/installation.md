# Sådan installerer du BPA Overblik

Programmet hentes som én fil til dit system. Du skal ikke installere Rust,
Python eller andre udviklerværktøjer.

Hent filen fra den nyeste udgivelse:
<https://github.com/Jdreioe/BPA_Overblik/releases/latest>

Udgivelserne hedder efter datoen, for eksempel `2026.09.21`.

| Dit system | Filen du skal bruge |
| --- | --- |
| Windows 10 eller 11, 64-bit | `teamup-shift-sync-ÅÅÅÅ.MM.DD-x86_64.exe` |
| macOS 11 eller nyere, både Intel og Apple Silicon | `teamup-shift-sync-ÅÅÅÅ.MM.DD-universal.pkg` |
| Linux, 64-bit | `teamup-shift-sync-ÅÅÅÅ.MM.DD-x86_64.AppImage` |

Google Chrome eller Chromium skal være installeret på maskinen i forvejen.
BPA Overblik henter aldrig selv en browser, og den rører aldrig din
almindelige browser: den åbner sit eget vindue til MitHF og DUOS.

## Windows

1. Gem `.exe`-filen et sted du kan finde igen, for eksempel i mappen
   **Dokumenter** eller på skrivebordet.
2. Dobbeltklik på filen.
3. Windows viser "Windows beskyttede din pc", fordi filen endnu ikke er
   signeret. Klik **Flere oplysninger** og derefter **Kør alligevel**.

Der er ikke noget at afinstallere: du sletter filen. Dine logins, dine kobler
mellem TeamUp, MitHF og DUOS og din historik ligger i
`%APPDATA%\teamup-shift-sync\data` og bliver ikke slettet med programmet.

## macOS

1. Dobbeltklik på `.pkg`-filen.
2. macOS siger, at pakken er fra en ikke-identificeret udvikler, fordi den
   endnu ikke er signeret. Højreklik i stedet på filen, vælg **Åbn**, og
   bekræft **Åbn** i vinduet der kommer frem.
3. Følg installationen. Programmet lægger sig i mappen **Programmer**.
4. Første gang du åbner **BPA Overblik**, spørger macOS igen. Vælg **Åbn**.

Du fjerner programmet ved at trække **BPA Overblik** fra Programmer til
papirkurven. Dine data ligger i `~/Library/Application Support/teamup-shift-sync`
og bliver ikke slettet med programmet.

## Linux

1. Gem `.AppImage`-filen et sted du kan finde igen, for eksempel i `~/Programmer`.
2. Gør filen kørbar: højreklik → **Egenskaber** → **Tilladelser** → sæt flueben
   ved at filen må køres som program. I en terminal svarer det til
   `chmod +x teamup-shift-sync-*.AppImage`.
3. Dobbeltklik på filen.

Der er ikke noget at afinstallere: du sletter filen. Dine data ligger i
`~/.local/share/teamup-shift-sync`.

## Hvad systemet spørger om

BPA Overblik gemmer adgangskoder i systemets egen nøglering, og det er den
eneste tilladelse den beder om:

- **macOS** spørger "BPA Overblik vil bruge oplysninger fra din nøglering".
  Vælg **Tillad**. Siger du nej, kan appen ikke huske dine logins.
- **Linux** bruger skrivebordets nøglering, for eksempel GNOME Keyring. Den
  skal være installeret og låst op; ellers beder appen dig om at logge ind
  hver gang.
- **Windows** bruger Legitimationsstyring og spørger ikke om noget.

Appen beder ikke om administratoradgang på Windows eller Linux. På macOS
spørger installationen én gang. Den kører ikke noget i baggrunden.

## Opdatering

Appen søger selv efter en nyere udgivelse, mens den er åbnet. I sidepanelet under
**Indstillinger** søger det runde pilikon igen. Når en ny version er klar, bliver
det til et downloadikon. Ikonet animerer, mens opdateringen hentes. Når den er
klar, bliver det til et genstartsikon. På Windows og Linux genstarter et klik
appen i den nye version. På macOS åbner klikket installationspakken, som du
følger som første gang. Dine logins,
dine kobler og din synkroniseringshistorik følger med over. Appen henter aldrig
vagter af sig selv.

## Hvis noget går galt

- **Programmet åbner ikke.** Prøv at åbne det igen. Windows og macOS spørger
  kun den første gang, men de spørger igen efter en opdatering.
- **"Chromium blev ikke fundet".** Installer Google Chrome eller Chromium og
  prøv igen. Har du en browser liggende et usædvanligt sted, kan stien sættes
  i `TEAMUP_BROWSER_PATH`.
- **Login virker ikke.** Klik **Log ind** igen. Den valgte uge og din opsætning
  går ikke tabt.
- **Noget andet.** Åbn **Support** i programmet og brug **Del hvad der gik galt**.
  Det åbner et færdigudfyldt opslag i din browser, som du selv læser igennem,
  før du sender det. Der står hverken navne, vagttekst, adgangskoder eller
  kalenderlink i det, men versionen af programmet står der, og den er det
  første jeg kigger på.
