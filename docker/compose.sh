#!/bin/bash

echo "database_url = \"$SMS_DATABASE_URL\"" > /sms-irc/config.toml
echo "insp_s2s.admin_nick = \"$SMS_ADMIN_NICK\"" >> /sms-irc/config.toml
echo "whatsapp.dl_path = \"$SMS_DL_PATH\"" >> /sms-irc/config.toml
cat /sms-irc/docker/compose-config.toml >> /sms-irc/config.toml
/sms-irc/sms-irc
