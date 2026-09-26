# Sådan installerer du BPA Overblik

Programmet hentes som én fil til dit system. Du skal ikke installere Rust,
Python eller andre udviklerværktøjer.

Hent filen fra den nyeste udgivelse:
<https://github.com/Jdreioe/BPA_Overblik/releases/latest>

Udgivelserne hedder efter datoen, for eksempel `2026.09.21`.

| Dit system | Filen du skal bruge |
| --- | --- |
| Windows 10 eller 11, 64-bit | `teamup-shift-sync-ÅÅÅÅ.MM.DD-x86_64-setup.exe` |
| macOS 11 eller nyere, både Intel og Apple Silicon | `teamup-shift-sync-ÅÅÅÅ.MM.DD-universal.pkg` |
| Linux, 64-bit | `teamup-shift-sync-ÅÅÅÅ.MM.DD-x86_64.AppImage` |

Programmet bruger en browser, der allerede er installeret: din
standardbrowser, hvis den er Chromium-baseret (Google Chrome, Brave, Vivaldi,
Opera, Arc eller Chromium), ellers Microsoft Edge, som følger med Windows. På
en Mac med kun Safari skal du først installere en Chromium-baseret browser.
BPA Overblik henter aldrig selv en browser, og den rører aldrig din
almindelige browser: den åbner sit eget vindue til MitHF og DUOS.

## Windows

1. Dobbeltklik på `-setup.exe`-filen.
2. Windows viser "Windows beskyttede din pc", fordi filen endnu ikke er
   signeret. Klik **Flere oplysninger** og derefter **Kør alligevel**.
3. Klik **Installer**. Du skal ikke være administrator.

Programmet lægger sig i din egen brugermappe og får en genvej i
**Start**-menuen og på skrivebordet. Installationsfilen kan du slette bagefter.

Du afinstallerer under **Indstillinger → Apps → Installerede apps → BPA
Overblik**. Dine logins, dine kobler mellem TeamUp, MitHF og DUOS og din
historik ligger i `%APPDATA%\teamup-shift-sync\data` og bliver ikke slettet
med programmet.

Brugte du før den enkelte `.exe`-fil uden installation, flytter appen selv
over til installationen ved næste opdatering og sletter den gamle fil.

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

## Vagtplan fra en kalender (iCal)

Under **Indstillinger → Udbydere → Vagtplan** kan du vælge **iCal-kalender** og
indsætte kalenderens iCal-link. Linket er hemmeligt, så appen gemmer det i
nøgleringen. Sådan finder du det:

- **Google Kalender:** Indstillinger → vælg kalenderen under *Indstillinger for
  mine kalendere* → *Integrer kalender* → kopiér *Hemmelig adresse i
  iCal-format*.
- **Outlook:** Indstillinger → Kalender → Delte kalendere → *Udgiv en
  kalender*. Vælg at alle detaljer kan ses, og kopiér ICS-linket.
- **iCloud:** Del kalenderen, slå *Offentlig kalender* til, og kopiér
  linket, der starter med `webcal://`.

Har hver hjælper sin egen kalender, så tilføj ét link pr. hjælper. Står alle
vagter i én fælles kalender, så vælg det. Hjælperens navn skal stå først i
titlen, før et tegn, for eksempel »Anna - Vagt«, eller udgøre hele titlen. Navnene, appen finder,
vises under **Hjælpere**.

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
klar, bliver det til et genstartsikon. På Windows lukker et klik appen,
installerer den nye version og åbner den igen. På Linux genstarter et klik
appen i den nye version. På macOS åbner klikket installationspakken, som du
følger som første gang. Dine logins,
dine kobler og din synkroniseringshistorik følger med over. Appen henter aldrig
vagter af sig selv.

## Vagtplan i et regneark

Under **Indstillinger → Udbydere → Vagtplan** vælger du **Regneark**. Indsæt
et link, eller klik **Vælg fil …** og vælg filen på computeren. Programmet
læser kun regnearket og ændrer aldrig noget i det.

Linket skal give alle med linket lov til at *se* regnearket:

- **Google Sheets:** Åbn fanen med vagtplanen. Klik **Del**, og vælg **Alle med
  linket** med rollen **Læser**. Kopiér derefter adressen fra browserens
  adressefelt.
- **Excel på OneDrive eller SharePoint:** Klik **Del**, og vælg **Alle med
  linket** og **Kan få vist**. Kopiér linket.
- **Nextcloud:** Klik på delingsikonet ved filen, og opret et **delingslink**.
  Lad det være skrivebeskyttet, og kopiér det.
- **Dropbox:** Klik **Del**, vælg **Alle med dette link kan se**, og kopiér
  linket.

Ligger filen i en mappe, der synkroniseres med OneDrive, Dropbox eller iCloud
Drive, kan du også bare vælge den med **Vælg fil …**. Programmet læser den
igen, hver gang du henter en uge.

Programmet læser `.xlsx`, `.ods` og `.csv`. Har du en ældre `.xls`-fil eller et
Numbers-regneark, så gem det som `.xlsx` først. Har regnearket flere faner,
spørger programmet, hvilken fane vagtplanen står i.

Skifter du til et andet link eller en anden fil, starter programmet en ny
historik. Kontrollér derfor den første uge ekstra grundigt.

## Hvis noget går galt

- **Programmet åbner ikke.** Prøv at åbne det igen. Windows og macOS spørger
  kun den første gang, men de spørger igen efter en opdatering.
- **"Ingen browser fundet".** Installer Google Chrome eller Microsoft Edge og
  prøv igen. Firefox og Safari kan ikke bruges. Har du en browser liggende et usædvanligt sted, kan stien sættes
  i `TEAMUP_BROWSER_PATH`.
- **Login virker ikke.** Klik **Log ind** igen. Den valgte uge og din opsætning
  går ikke tabt.
- **Noget andet.** Åbn **Support** i programmet og brug **Del hvad der gik galt**.
  Det åbner et færdigudfyldt opslag i din browser, som du selv læser igennem,
  før du sender det. Der står hverken navne, vagttekst, adgangskoder eller
  kalenderlink i det, men versionen af programmet står der, og den er det
  første jeg kigger på.
