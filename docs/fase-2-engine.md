# PILOT — Fase 2: audio-engine

Status: **gebouwd en uitgerold op de testserver** · 28-11-2026

De engine is één Rust-programma (`engine/`, binary `pilot`) dat de stream ophaalt, het geluid
bewerkt, het complete MPX-signaal maakt en dat naar de uitgangen stuurt. Alles is instelbaar via
de API op poort 8080 ([api.md](api.md)); de webinterface van fase 3 bouwt daarop.

## Signaalketen

```
Icecast (MP3/AAC/HE-AAC) ─▶ decoder ─▶ resampler 48 kHz + driftcorrectie ─▶ buffer
48 kHz:  trim ▶ DC-filter ▶ AGC ▶ 5-band compressor (Linkwitz-Riley, fasegecompenseerd)
         ▶ 4× oversampling
192 kHz: pre-emphasis 50/75 µs ▶ look-ahead limiter ▶ soft clipper ▶ 15 kHz-laagdoorlaat (≥ 60 dB bij 19 kHz)
         ▶ M/S ▶ stereo-encoder (38 kHz) ▶ BS.412-powerlimiter ▶ composiet-veiligheidslimiter
         ▶ + pilot 19 kHz ▶ + RDS 57 kHz (+ RDS2 66,5 / 71,25 / 76 kHz)
         ▶ MicroMPX-loopback · geluidskaart · SDR · stream
```

## Limieten: zo blijft de zender binnen zijn stokken

- **75 kHz piekdeviatie.** Het audiodeel van het composiet wordt door een look-ahead
  piekbegrenzer gehouden op `max − pilot − RDS`. Pilot en RDS worden daarna opgeteld, dus de som
  kan nooit boven 100 % komen. Een harde clip op 100 % is de laatste vangrail; de teller
  `hard_clips` hoort op 0 te blijven (in alle tests is dat zo).
- **+3 dBr MPX-power (ITU-R BS.412).** Gemeten over een glijdend venster van 60 seconden,
  inclusief pilot en RDS. Een trage regelaar stuurt naar 0,3 dB onder de grens; een snelle
  regelaar rekent per blok uit hoeveel energie er nog mag en grijpt direct in als het venster
  anders over de grens zou gaan.
- Via de API kun je beide grenzen alleen **lager** zetten, nooit hoger.

## RDS

- Groepen 0A (PS, AF methode A, DI-stereo, TP/TA, PTY, M/S), 2A (RadioText met A/B-vlag),
  4A (klok, elke minuut).
- Bitklok afgeleid van de pilot (1 bit = 16 pilotperioden), draaggolf 57 kHz = 3 × pilot,
  biphase-symbolen met het cosinusfilter uit IEC 62106.
- **Dynamische RadioText** uit de `StreamTitle` van de stream, met sjabloon
  (bijv. `Nu: {artist} - {title}`). Handmatig zetten kan via `POST /api/rds/radiotext`.
- **RDS2 (experimenteel, standaard uit):** drie extra subdraaggolven met hetzelfde soort
  groepsdata. Stationslogo's (RFT) volgen later.
- De test `modulated_rds_decodes_back` demoduleert ons eigen signaal en leest de PS-naam terug.

## Invoer

- `decoder: auto` gebruikt **ffmpeg** (MP3, AAC-LC, HE-AAC v1/v2, Ogg, Opus). `native` gebruikt de
  ingebouwde Symphonia-decoder (MP3 en AAC-LC; HE-AAC klinkt daarmee dof, dus `auto` is de keuze).
- Valt de hoofdstream langer dan `failover_s` weg, dan schakelt PILOT naar de back-upstream en
  daarna naar het terugvalbestand. Elke minuut kijkt hij of de hoofdstream terug is.
- Een buffer (standaard 1,5 s) plus driftcorrectie in de resampler vangt het klokverschil
  tussen streamserver en onze uitgang op, zonder klikken.

## Uitgangen

| Uitgang | Werking |
|---|---|
| **MicroMPX** | MPX op 192 kHz naar de ALSA-loopback (`hw:Loopback,0,0`). De officiële Thimeo-encoder leest de andere kant (`hw:Loopback,1,0`) en stuurt UDP naar de uMPX. Deze uitgang is de **masterklok**. Optioneel pollt PILOT de webinterface van de encoder. |
| **Geluidskaart** | `mpx`: composiet op 192 kHz (DC-gekoppelde MPX-kaart). `audio`: bewerkte L/R op 48 kHz (monitoring). |
| **SDR** | FM-gemoduleerde I/Q (cs8/cs16/cf32) via stdin naar bijv. `hackrf_transfer`. Alleen voor tests op een dummy load: uitzenden vereist een vergunning. |
| **Stream** | Bewerkte audio (de-emphasis toegepast) via ffmpeg naar een Icecast-mount, MP3 of AAC. |

Is er geen apparaat beschikbaar, dan loopt de engine op de systeemklok door.

## Testen zonder hardware

```
pilot render muziek.mp3 mpx.wav --seconds 60
```

Maakt een MPX-WAV van 192 kHz en drukt piekdeviatie, MPX-power, LUFS en het aantal harde clips af.

## Installatie (Debian 13, native)

```
git clone https://github.com/rhisijmuiden/fm-mpx-processor /opt/pilot
sudo /opt/pilot/deploy/install.sh --channel test
```

Het script installeert ffmpeg, ALSA en de Rust-toolchain uit Debian, bouwt de engine, maakt de
gebruiker `pilot`, laadt `snd-aloop` en start `pilot.service` (realtime-prioriteit, beheer op 8080).
Let op: de Debian-**cloud**kernel heeft geen geluidsmodules; installeer `linux-image-amd64`.

## Testserver

| | |
|---|---|
| VM | `pilot-test`, VMID 109 op pve01 (Arie-Oud) |
| Netwerk | VLAN 4, 10.100.4.110/24, gateway 10.100.4.254 |
| Systeem | Debian 13, 4 vCPU, 4 GB, 32 GB, generieke kernel met `snd-aloop` |
| Beheer | http://10.100.4.110:8080 (tijdelijke statuspagina; de webinterface volgt in fase 3) |
| Uitrol | StackPilot-deployment `pilot-test` (kanaal test → branch `test`) |

## Nog open

- Thimeo MicroMPX-encoder installeren zodra de licentie er is (proefversie kan al).
- Kalibratie van het MPX-niveau met de DB45 (uitgangsniveau per uitgang instelbaar).
- Login en HTTPS komen met de webinterface (fase 3); tot dan kan `PILOT_API_TOKEN` de
  schrijfacties afschermen.
