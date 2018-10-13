# sms-irc

![GNU AGPLv3 licensed](https://www.gnu.org/graphics/agplv3-155x51.png)

A WhatsApp Web and SMS bridge for [internet relay chat (IRC)](https://en.wikipedia.org/wiki/Internet_Relay_Chat). (beta!)

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

## How do I set it up?

Documentation is currently being worked on, and will be available soon! If you're really keen, you can get in touch with
[eta](https://theta.eu.org), who should be able to help you set it up.
