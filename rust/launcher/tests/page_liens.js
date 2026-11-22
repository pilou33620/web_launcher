// Liens des outils sur la page du launcher (urlOutil de web/js/app.js), sans navigateur.
// Les outils du Pi n'écoutent que sur 127.0.0.1 : Ouvrir, Copier et Adresse donnent le relais public.
//   node rust/launcher/tests/page_liens.js
"use strict";
const assert = require("assert");
const fs = require("fs");
const path = require("path");
const vm = require("vm");

const src = fs.readFileSync(path.join(__dirname, "../../../web/js/app.js"), "utf8");
const debut = src.indexOf("function hoteOutils()");
const fin = src.indexOf("function dateCourte(");
assert(debut > 0 && fin > debut, "hoteOutils … urlOutil introuvables dans web/js/app.js");

function lien(page, launcher) {
  const ctx = { location: new URL(page), etat: { launcher } };
  vm.runInNewContext(src.slice(debut, fin) + "\nresultat = urlOutil({ id: 'web_menu', port: 8080 });", ctx);
  return ctx.resultat;
}

const ts = { nom_tailscale: "pilou.tail619296.ts.net", port_public_relais: 8443, ip: "192.168.1.20" };
const relais = "https://pilou.tail619296.ts.net:8443/o/web_menu/";
const cas = [
  ["http://192.168.1.20/", ts, relais],                  // réseau local, par l'IP
  ["https://192.168.1.20:80/", ts, relais],              // réseau local en https (port gardé)
  ["http://launcher.local/", ts, relais],                // réseau local, par le nom
  ["http://127.0.0.1/", ts, relais],                     // sur le Pi lui-même
  ["https://pilou.tail619296.ts.net/", ts, relais],      // par Funnel
  ["https://pilou.tail619296.ts.net/", { nom_tailscale: "" }, relais], // Funnel, nom pas encore connu
  ["http://192.168.1.20/", { nom_tailscale: "", ip: "192.168.1.20" }, "http://192.168.1.20:8080/"], // sans Tailscale
  ["http://127.0.0.1/", { nom_tailscale: "" }, "http://127.0.0.1:8080/"],
];
for (const [page, launcher, attendu] of cas) {
  assert.strictEqual(lien(page, launcher), attendu, page);
}
console.log(`liens des outils : ${cas.length} cas OK`);
