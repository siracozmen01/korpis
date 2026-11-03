**Türkçe** · [English](./README.md)

# Korpis

**Kendi kontrolündeki makineler için bir iş yükü platformu.**

Korpis, yaşam döngüsü olan her şeyi (bir oyun sunucusu, bir bot, bir web servisi, bir veritabanı,
zamanlanmış bir görev, bir sanal makine) senin donanımında çalıştırır. İş yükünü bir düğüme
yerleştirir, istediğin kadar güçlü izole eder, depolama ve ağ verir, beyan ettiğin durumda tutar;
ve hem senin hem de yetki devrettiğin kişilerin bunu bir panelden, terminalden, sohbetten veya
API'den işletmesine izin verir.

> **Henüz yayımlanmadı.** Yapımın Phase 0'dan 3'e kadarı `crates/` (Rust) ve `web/` (TypeScript)
> altında tamamlandı: tek bir makinedeki tek bir operatör onu kurar, iş yüklerini çalıştırır,
> yedekler ve geri yükler, panelden ya da CLI'dan dürüst bir konsol okur; gösterdiği her sınır
> çekirdek tarafından uygulanır. Başkalarına barındırma yapar: organizasyonlar altlarındaki
> organizasyonlara devralınan kotayla yetki devreder, kişiler kendi kimlik sağlayıcılarıyla, onlar
> için hiçbir hesap açılmadan oturum açar ve her kiracının kullandığı ölçülür. Ve birçok düğüm
> çalıştırır: bir iş yükü birimleri, genel adresi ve ölçümüyle birlikte düğümler arasında taşınır,
> bir konteynerin yeterli izolasyon olmadığı yerde bir mikro VM'de çalışır.
> [`scripts/e2e-phase0.sh`](./scripts/e2e-phase0.sh)'den [`e2e-phase3.sh`](./scripts/e2e-phase3.sh)'e
> kadarki betikler bunu bir operatörün yapacağı gibi doğrular; sırada ne olduğunu
> [kilometre taşları](https://github.com/siracozmen01/korpis/milestones) söyler.

---

## Neyi farklı yapar

**Beyan eder, asla emretmez.** Neyin doğru olması gerektiğini sen söylersin. Korpis gerçekliği
sürekli olarak ona yaklaştırır ve gerçekte neyin doğru olduğunu bildirir. Hiçbir bileşen bir
diğerine bir şey yapmasını söyleyip işe yaradığını varsaymaz; "panel durdu diyor ama sunucu
çalışıyor" tam olarak buradan gelir.

**Farkı gerçek bir nesne yapar.** Her değişiklik, bir etki üretmeden önce incelenebilir bir plan
üretir. Deneme çalıştırmaları, onaylar, zamanlanmış değişiklikler, geri alma ve yalan söyleyemeyen
bir denetim izi, beş ayrı özellik değil, bu tek kararın sonuçlarıdır.

**Rolleri yoktur.** Yetki bir *grant*'tir (bir özne, bazı eylemler, bir kapsam, bazı koşullar) ve bir
grant yalnızca daha zayıf çocuklar üretebilir. Böylece "bu bağlantı, arkadaşının bu tek sunucuyu
önümüzdeki 24 saat boyunca, hesap gerekmeden yeniden başlatmasına izin verir" sıradan bir işlemdir ve
bayilik, birinin yazdığı bir özellik değil, modelin bedavaya yaptığı bir şeydir.

**İzolasyonu seçmene izin verir.** İş yükü başına `process`, `container`, `microvm`, `vm`, tek bir
sürücü arayüzünün arkasında. Her katman, neyi durdurduğu kadar neyi *durdurmadığıyla* da belgelenir;
çünkü izolasyon gibi görünüp olmayan bir katman, dürüstçe daha zayıf diye etiketlenmiş olandan
kötüdür.

**Uygulamadığı şeyi asla iddia etmez.** Gösterilen her sınır çekirdek tarafından uygulanır ve
ölçülür. Her durum gözlemlenmiştir. Korpis bir şeyi göremediğinde tahmin etmek yerine `unknown`
der; veri düşürdüğünde temiz görünen bir boşluk yerine işaretli bir boşluk bırakır.

**Büyümeden vergi almaz.** İş yükü, düğüm, örnek, hesap veya alan adı başına hiçbir ücret yok, hiçbir
ölçekte, hiçbir zaman. Sürüm ayrımı yok, kilitli özellik yok, ticari kullanım kısıtlaması yok.

---

## Buradan başla

| Yapmak istediğin | Oku |
|---|---|
| ne olduğunu ve parçaların nasıl birleştiğini anlamak | [overview](./docs/internals/overview.md), sonra [glossary](./docs/internals/glossary.md) |
| kurmak ve düğüm eklemek | [install](./docs/user/install.md), [nodes](./docs/user/nodes.md), [recovery](./docs/user/recovery.md) |
| güvenli olup olmadığını bilmek | [security](./docs/internals/security.md), [grants](./docs/internals/grants.md), [runtimes](./docs/internals/runtimes.md) |
| üzerine bir şey inşa etmek | [api](./docs/internals/api.md), [extensions](./docs/internals/extensions.md), [recipes](./docs/internals/recipes.md) |
| kod üzerinde çalışmak | [CONTRIBUTING.md](./CONTRIBUTING.md), [development](./docs/operations/development.md), [architecture](./docs/internals/architecture.md) |
| kimin karar verdiğini bilmek | [GOVERNANCE.md](./GOVERNANCE.md) |

```
docs/internals/    kodun bağlı olduğu tasarım: her konu için yaşayan tek bir sayfa
docs/user/         kurulum ve işletim, göreve göre
docs/operations/   geliştirme, denetim ve yayımlama
crates/            uygulama, bir Rust çalışma alanı
web/               panel, statik bir TypeScript paketi
AGENTS.md          depoyu değiştiren herkesin uyduğu kurallar
```

Karara bağlanmamış tasarım soruları [`question` etiketli issue'lardır](https://github.com/siracozmen01/korpis/issues?q=is%3Aissue+label%3Aquestion).
Üzerinde bir isim olan ertelenmiş bir karar dürüsttür; beyan edilmemiş olan ise birinin sonradan
düşeceği bir tuzaktır.

---

## Bu nasıl yazılıyor

**Tek bir kişi tarafından, Claude ile.** Bunu birinin fark etmesine bırakmak yerine açıkça söylemeye
değer. Kimse bir tasarıma, onu kimin ya da neyin ürettiğine bakarak güvenmemeli; bu yüzden:

- **Yaygın uygulamadan her sapma, alıntılanabilir bir şeye dayanarak savunulur ya da bir karar
  olduğunu söyler.** Başka projeler hakkındaki iddialar, ilgili issue'ya, güvenlik bildirimine veya
  belgeye satır içinde bağlantı verir.
- **Tasarım bilerek saldırıya uğradı.** Somut senaryolar dokundukları her katman boyunca izlendi,
  topun kimsede olmadığı adım arandı; bağımsız incelemeler her tanımı "bunu ne, nerede, hangi
  bilgiyle değerlendiriyor?" diye okudu. Bulunan her kusur tasarımda ve kodda düzeltildi ve git
  geçmişinde duruyor.
- **Her sınır, uygulandığı yerde test edilir.** Testler gerçek PostgreSQL, gerçek bir kayıt defteri,
  gerçek containerd ve gerçek microVM'lere karşı çalışır; panel, gerçek bir kontrol düzlemine karşı
  gerçek bir tarayıcıda sürülür.

Muhakeme, öncelikler ve hatalar yazarındır. Burada bir şey yanlışsa bir issue aç ve söyle.

---

## Katkı

Herkesin yapabileceği en faydalı şey **onunla tartışmak**: bir akıl yürütme boşluğu, belirtilmemiş bir
varsayım, kimsenin izlemediği bir arıza biçimi, başka bir proje hakkında yanlış ya da haksız bir
iddia, ya da bu sınıf yazılımı ölçekte işletme deneyimi. Bkz. [CONTRIBUTING.md](./CONTRIBUTING.md).
Bir güvenlik açığı bildirmek için: [SECURITY.md](./SECURITY.md).

---

## Lisans

| Katman | Lisans |
|---|---|
| Protokol tanımları, istemci SDK'ları, recipe biçimi, ajan protokolü, uygunluk paketleri | **Apache-2.0** |
| Kontrol düzlemi, düğüm ajanı, aktarıcı (relay), panel, CLI ve sohbet istemcileri | **AGPL-3.0-or-later** |
| Belgeler | **CC BY 4.0** |
| Recipe'ler, eklentiler, temalar | Yazarın seçimi, kapalı kaynak dahil |

Açıkça, çünkü AGPL bir kullanım kısıtlaması diye yanlış okunur: **ticari olarak çalıştır, üzerinde
kapasite sat, istediğini ücretlendir, ona karşı kapalı eklentiler yaz.** Onu müşterilerin için
değiştirmeden çalıştırmak hiçbir yükümlülük getirmez. Değiştirip değiştirilmiş sürümü bir ağ
üzerinden sunmak, kullanıcılarına kaynak kodunu sunmak demektir; kopya dağıtmak, kaynağın onlarla
gitmesi demektir ve değiştirilmemiş bir derleme bunu zaten sağlar.

Katkılar, CLA ve telif devri olmadan, bir Developer Certificate of Origin altında gelir; böylece kimse
bir katkıcının kodunu onun rızası olmadan yeniden lisanslayamaz. Lisans tek başına açık çekirdekli
(open-core) bir dönüşü engellemez; bunu projenin politikası engeller ve
[GOVERNANCE.md](./GOVERNANCE.md) hangisinin hangisi olduğunu söyler.
