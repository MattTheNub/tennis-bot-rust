#!/usr/bin/env bash
set -euo pipefail

if [[ $EUID -ne 0 ]]; then
    echo 'Run this installer with sudo.' >&2
    exit 1
fi

artifact_dir=$(realpath "${1:?Usage: install.sh ARTIFACT_DIRECTORY}")
for artifact in tennis-bot tennis-bot.env tennis-bot.service; do
    test -s "$artifact_dir/$artifact"
done

if ! id tennis-bot >/dev/null 2>&1; then
    useradd --system --user-group --no-create-home --shell /usr/sbin/nologin tennis-bot
fi

release_id="$(date -u +%Y%m%dT%H%M%SZ)-$$"
release_dir="/opt/tennis-bot/releases/$release_id"
install -d -m 0755 /opt/tennis-bot /opt/tennis-bot/releases "$release_dir"
install -m 0755 "$artifact_dir/tennis-bot" "$release_dir/tennis-bot"
install -m 0600 "$artifact_dir/tennis-bot.env" /etc/tennis-bot.env
install -m 0644 "$artifact_dir/tennis-bot.service" /etc/systemd/system/tennis-bot.service

# Switch the executable atomically and retain previous release directories.
ln -s "$release_dir" "/opt/tennis-bot/.current-$$"
mv -Tf "/opt/tennis-bot/.current-$$" /opt/tennis-bot/current
systemd-analyze verify /etc/systemd/system/tennis-bot.service
systemctl daemon-reload
systemctl enable tennis-bot.service
systemctl restart tennis-bot.service
printf 'Installed release %s\n' "$release_id"
