#!/bin/sh
set -eu

test -f /run/openconnect/vpn-ready
test -n "$(ss -H -ltn 'sport = :1080')"
