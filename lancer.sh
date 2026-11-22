#!/bin/sh
# Ancien point d'entrée, gardé pour les services déjà installés : voir demarrer_WEB_LAUNCHER.sh.
exec sh "$(dirname "$0")/demarrer_WEB_LAUNCHER.sh" "$@"
