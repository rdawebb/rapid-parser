# Datasets

Bench corpora, not committed (everything but this file is ignored).

`cargo bench --bench ingest` skips any that are missing.

## Enron (`maildir/`)

The CMU release, about 517k messages in 1.4 GB: 150 users' folders, one file
per message.

```sh
curl -O https://www.cs.cmu.edu/~enron/enron_mail_20150507.tar.gz
tar -xzf enron_mail_20150507.tar.gz -C datasets
```

## Apache httpd dev list (`httpd-dev.mbox`)

`dev@httpd.apache.org`, 2015–2019: about 12k messages in 143 MB, one mbox
made of the 60 monthly archives.

```sh
for y in 2015 2016 2017 2018 2019; do for m in 01 02 03 04 05 06 07 08 09 10 11 12; do
  curl -sf "https://lists.apache.org/api/mbox.lua?list=dev@httpd.apache.org&date=$y-$m"; echo
done; done > datasets/httpd-dev.mbox
```
