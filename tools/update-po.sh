#!/bin/sh
# Extracts the translatable strings into po/atlas.pot and merges them into
# every po/<lang>.po. Run inside the GNOME SDK (xgettext knows Rust).
set -e
cd "$(dirname "$0")/.."
xgettext --language=Rust --from-code=UTF-8 --keyword=tr --keyword=ntr:1,2 \
    --add-comments=TRANSLATORS --package-name=atlas -o po/atlas.pot \
    $(find atlas/src -name '*.rs' | sort)
for lang in $(cat po/LINGUAS); do
    msgmerge --update --backup=none "po/$lang.po" po/atlas.pot
done
