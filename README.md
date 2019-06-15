# sms-irc

[![GNU AGPLv3 licensed](https://www.gnu.org/graphics/agplv3-155x51.png)](https://www.gnu.org/licenses/agpl-3.0.en.html)
[![GitHub stars](https://img.shields.io/github/stars/eeeeeta/sms-irc.svg?style=social)](https://github.com/eeeeeta/sms-irc)
[![IRC chat @ #sms-irc on freenode](https://img.shields.io/badge/irc-%23sms-irc%20on%20freenode-blue.svg)](https://kiwiirc.com/client/chat.freenode.net/#sms-irc)
![Maintenance](https://img.shields.io/maintenance/yes/2019.svg)

A WhatsApp Web and SMS bridge for [internet relay chat (IRC)](https://en.wikipedia.org/wiki/Internet_Relay_Chat). (slightly less beta!)

## What is this?

This monstrosity lets **one** user send and receive SMS messages through IRC, using a USB 3G modem plugged in to
the server running it. It also has integrated support for WhatsApp, using the [whatsappweb-rs](https://github.com/eeeeeta/whatsappweb-rs)
library. Using it requires running your own IRC daemon, or having a friendly IRC daemon somewhere that lets you
make large numbers of connections from one IP address.

It also has support for the [InspIRCd](http://www.inspircd.org/) spanning-tree protocol (v1.2), allowing you to
link it to an IRC network as a pseudo-server. (This is the configuration used by the author, and is probably
the most reliable way to use the bridge.)

This is also the spiritual successor of [matrix-appservice-sms](https://github.com/eeeeeta/matrix-appservice-sms/),
in that it does the same thing as matrix-appservice-sms, except way more reliably and for a different protocol.

## What can it do?

- Send and receive SMS messages through IRC, using a USB 3G modem
  - Deals with concatenated (longer) SMS messages as well!
  - Reasonably tolerant of modem flakiness
- Send and receive WhatsApp messages through IRC
  - Deals with both one-to-one chats and groupchats
- Receive WhatsApp attachments and deal with them nicely
  - Saves them in a folder somewhere for you to point a webserver at
- Manage SMS and WhatsApp contacts, allowing you to give people nice nicknames
  - Lets you switch between texting and WhatsApping people
  - Automatically tries to detect which method is best
- Manage the whole thing through a 'nice' IRC interface
  - Imitates NickServ/ChanServ-style commands
- Links to InspIRCd as a 'services'-type pseudoserver
  - Also has an undocumented and probably broken "spawn a million clients" mode, but we don't talk about that

## What does it need to run?

At minimum, you need:

- An [InspIRCd](http://www.inspircd.org/) server, running version 2.0 or later. (3.0 supported!)
- A [PostgreSQL](https://www.postgresql.org/) database, running version 9 or later.

If you want WhatsApp or attachments to work, you probably also need:

- A web server, like nginx, apache, or whatever, that you can point at sms-irc's attachments directory.
  - Should be accessible from the devices you want to use sms-irc with.

If you want to use the SMS stuff, you need:

- A USB 3G modem, like the Huawei E3531, or Huawei E169.
  - The Huawei E3531 is the author's preferred modem, and what they've managed to get it working with.
  - Other modems are available, and may well work, provided they follow the AT/Hayes command set.

**Warning:** sms-irc is not quite yet plug-and-play. You are expected to know a bit about how databases and
web servers work in order to get everything connected together. In the future, things will be made easier.

*In particular, the docker-compose setup method mentioned below lets you avoid having to worry about the IRCd.
You'll still need to configure Postgres and a web server, though.*

## How do I set it up?

To build this thing, you need a decently recent stable version of [Rust](https://www.rust-lang.org/), and development
libraries for PostgreSQL (`pacman -S postgresql-libs` on Arch). Then, it's as simple as

```
$ cargo build --release
```

to *build* it. Configuring it takes some more effort - read `config.toml.example` for the gory details, and rename it
to `config.toml` when done. Then

```
$ cargo run --release
```

should get you up and running.

## What if I'm lazier than that?

Good news! We have a [Docker](https://www.docker.com/) container for you, as [`eeeeeta/sms-irc`](https://hub.docker.com/r/eeeeeta/sms-irc)
on Docker Hub.

### Fancy docker compose method (recommended!)

If you [install Docker Compose](https://docs.docker.com/compose/install/), you can get up and running pretty quickly
with a pre-configured copy of sms-irc and InspIRCd 3. You'll need to define four environment variables:

- `SMS_DATABASE_URL`: a PostgreSQL database URL to use for the database.
  - This is in the form `postgresql://user[:password]@host/dbname`.
  - Keep in mind this connection will be made from within a Docker container, so your Postgres will need to be reachable that way.
- `SMS_ADMIN_NICK`: the nickname you're going to connect as (e.g. `eeeeeta`)
- `SMS_ATTACH_PATH`: a path to somewhere on your server to store attachments (gets mounted in the container)
- `SMS_DL_PATH`: a URL fragment for attachments, which will get at the files in `SMS_ATTACH_PATH`
  - Basically, this is all explained in `config.toml.example`, but if a file is saved as `SMS_ATTACH_PATH/file.txt`,
    sms-irc expects to hand out URLs like `SMS_DL_PATH/file.txt` to IRC clients, and it's your job to make some web server
    make that all happen.
  - For local use, you might want to just use a `file://` URI pointing at your `SMS_ATTACH_PATH`.

### Simple but hard method

If you're more hardcore, you can also use the Docker image directly, for example:

```
$ docker run --name sms-irc \
	-v PATH_TO_STORE_DATA_AND_CONFIGS_IN:/data \
	-e "SMSIRC_CONFIG=/data/config.toml" \
	eeeeeta/sms-irc
```

This helps you avoid the building part - you'll want to provide it with a `PATH_TO_STORE_DATA_AND_CONFIGS_IN` where you'll put your
`config.toml`, and under which you'll also want to store your data directories.

## Aaaagh what, this is all very confusing and I have questions

Feel free to join [`#sms-irc` on irc.freenode.net](https://kiwiirc.com/client/chat.freenode.net/#sms-irc)
and give [eta](https://theta.eu.org) a hard time about how hard their software is to install.
