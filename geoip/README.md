# Bundled GeoIP database

`dbip-country-lite.mmdb` is DB-IP's free [IP to Country Lite](https://db-ip.com/db/download/ip-to-country-lite)
database in MaxMind format. Vaka uses it to turn client IP addresses into country codes; the
addresses themselves are never stored.

It is refreshed monthly by `.github/workflows/geoip.yml`, and running servers check for a newer
release once a week (`VAKA_GEOIP_UPDATE`). See [self-hosting](../docs/self-hosting.md#country-lookup).

License: [Creative Commons Attribution 4.0 International](https://creativecommons.org/licenses/by/4.0/).
[IP Geolocation by DB-IP](https://db-ip.com).
