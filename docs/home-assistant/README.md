# Setting up Home Assistant, and the phone panel

Two files go with this guide:

- [`phone-panel/index.html`](../../phone-panel/index.html) — a web dashboard
  for phones. One file, no build step, no outside requests. It talks to Home
  Assistant on the same address it is served from.
- [`docker-compose.yml`](docker-compose.yml) — only needed for the Docker
  route below.

## 1. Install Home Assistant on the Windows server

### Recommended: Home Assistant OS in a Hyper-V virtual machine

Docker on Windows runs Linux containers inside a hidden VM with its own
network, so Home Assistant cannot see the broadcast traffic most smart devices
announce themselves with. You would be adding nearly every device by IP address,
and you get no add-on store. Hyper-V is built into Windows Server and avoids
both problems.

1. Enable the **Hyper-V** role (Server Manager → Add roles and features).
2. In Hyper-V Manager → **Virtual Switch Manager**, create an **External**
   switch bound to the server's wired network adapter. This puts the VM
   directly on your home network.
3. Download the Hyper-V image (`haos_ova-….vhdx.zip`) from
   <https://www.home-assistant.io/installation/windows> and unzip it somewhere
   permanent.
4. New → Virtual Machine: **Generation 2**, 4 GB memory (2 GB minimum),
   connect to the external switch, **Use an existing virtual hard disk** → the
   `.vhdx`.
5. Before first start, in the VM's Settings: Security → **untick Secure Boot**;
   Processor → 2 virtual processors; Automatic Start Action → **Always start**.
6. Start it, wait a few minutes, then open `http://homeassistant.local:8123`
   (or the IP shown in the VM console) and create your account.
7. In your router, reserve that IP address for the VM so it never changes.

One limit: Hyper-V cannot pass a USB stick through to the VM. If you have
Zigbee or Z-Wave devices that need a USB radio, buy a network-attached
coordinator (for example an SLZB-06 for Zigbee) rather than a USB one.

### Alternative: Docker

Works, with the discovery limits above. Put this folder on the server and run
`docker compose up -d`, then open `http://<server-ip>:8123`. Note that Docker
Desktop is not supported on Windows Server editions; this route is realistic
mainly on Windows 10/11 or a Linux box.

## 2. Add your devices

After onboarding, Home Assistant lists what it found under **Settings →
Devices & services**. Press *Configure* on each one. For anything not found,
use **Add integration** and search for the brand (Hue, Kasa, Tuya, Nest,
ecobee, Ring, Sonos, Roborock, and so on). Cloud brands will ask you to sign in.

As you add each device, **assign it to an Area** (Kitchen, Bedroom…). The panel
groups its cards by those areas.

## 3. Install the panel

1. Copy `phone-panel/index.html` from this repository into Home Assistant's
   config folder so it sits at `config/www/panel/index.html`.
   - Hyper-V route: install the **Samba share** or **File editor** add-on
     (Settings → Add-ons) to reach the config folder.
   - Docker route: it is the `config` folder next to `docker-compose.yml`.
2. If the `www` folder did not exist before, **restart Home Assistant once**
   (Settings → System → Restart). It only starts serving that folder at boot.
3. Create the access token: click your name (bottom left) → **Security** →
   **Long-lived access tokens** → *Create token*. Copy it; it is shown once.
4. Open `http://<home-assistant-address>:8123/local/panel/index.html` on your
   phone, paste the token, press **Connect**.
5. On the phone, use the browser's **Add to Home Screen** so it opens like an
   app.

Using the panel:

- **By room / By type** switches the grouping.
- **Customize** chooses which devices appear (it starts with lights, plugs,
  climate, locks, blinds, media, vacuums, cameras, door/motion sensors and
  temperature/humidity readings) and holds the e-ink switch. These choices are
  stored per device, so each phone or tablet can show a different set.
- Unlocking a door asks for confirmation; everything else acts on one tap.

About the token: it is stored in that browser only, and the page itself contains
no secrets. A token made from your own account can do anything you can. Grouping
by room needs an administrator token; a token from a non-admin user still works
but the panel falls back to grouping by type.

## 4. Reaching it away from home

Do not forward port 8123 on your router. Either subscribe to Home Assistant
Cloud (Settings → Home Assistant Cloud), or install Tailscale on the server/VM
and on your phone and keep using the same address.

## 5. The Kobo

The Kobo does not use this web page. It runs the native dashboard in this
repository, which draws straight to the e-ink screen; see the
[main README](../../README.md) for installing it.
