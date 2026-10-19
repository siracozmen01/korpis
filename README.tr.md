<!-- Translation of README.md at 57ee44a. Update this hash when you re-sync. -->

**Türkçe** · [English](./README.md)

# Korpis

**Kendi kontrolündeki makineler için bir iş yükü platformu. Tasarım spesifikasyonu.**

Korpis, yaşam döngüsü olan her şeyi (bir oyun sunucusu, bir bot, bir web servisi, bir veritabanı,
zamanlanmış bir görev, bir sanal makine) senin donanımında çalıştırır. İş yükünü bir düğüme
yerleştirir, istediğin kadar güçlü izole eder, depolama ve ağ verir, beyan ettiğin durumda tutar;
ve hem senin hem de yetki devrettiğin kişilerin bunu bir panelden, terminalden, sohbetten veya
API'den işletmesine izin verir.

> **Önce tasarım geldi, kod ilk iki aşamasını tamamladı.** Yapım sırasının Phase 0 ve Phase 1'i
> `crates/` (Rust, [`21-stack.md`](./docs/21-stack.md) §3) ve `web/` altında kuruldu: tek bir
> makinedeki tek bir operatör onu kurar, iş yüklerini çalıştırır, yedekler ve geri yükler, dürüst
> bir konsol okur; panelden ya da CLI'dan, gösterdiği her sınır çekirdek tarafından uygulanarak.
> [`scripts/e2e-phase0.sh`](./scripts/e2e-phase0.sh) ve
> [`scripts/e2e-phase1.sh`](./scripts/e2e-phase1.sh) bunu bir operatörün yapacağı gibi doğrular.
> Henüz başkalarına barındırma için değil: onların organizasyonları, yetki devri ve ölçümleme
> Phase 2'de gelir.
>
> Yirmi dört doküman var ve kod onlara göre yazılıyor. Böyle bir sistemde bazı kararlar sonradan
> takılamaz: durumun nasıl saklandığı, yetkinin nasıl devredildiği, güvenlik sınırının nerede
> durduğu, ajan protokolünün ne söz verdiği. Bunları kâğıt üstünde yanlış yapmanın bedeli bir
> paragraf. Dokuzuncu ayda yanlış yapmanın bedeli baştan yazmak, ki bu yazılım kategorisinin başına
> birden fazla kez gelmiş bir şey.

---

## Farkı ne

**Emretmez, beyan eder.** Neyin doğru olması gerektiğini söylersin. Korpis gerçekliği sürekli
olarak oraya yakınsatır ve gerçekte neyin doğru olduğunu bildirir. Hiçbir bileşen bir diğerine bir
şey yapmasını söyleyip işe yaradığını varsaymaz; "panel durdu diyor ama sunucu çalışıyor" tam
olarak oradan gelir.

**Farkı gerçek bir nesne yapar.** Her değişiklik, bir `Effect` üretmeden önce incelenebilir bir
`Plan` üretir. Kuru çalıştırma, onaylar, zamanlanmış değişiklikler, geri alma ve yalan söyleyemeyen
bir denetim izi; bunlar beş ayrı özellik değil, tek bir kararın sonuçları.

**Rol diye bir şeyi yok.** Yetki bir *grant*'tir (bir özne, birkaç eylem, bir kapsam, birkaç koşul)
ve bir grant yalnızca kendinden zayıf çocuklar üretebilir. Böylece "bu bağlantı arkadaşının
önümüzdeki 24 saat boyunca sadece bu sunucuyu yeniden başlatmasına izin verir, hesap gerekmez"
sıradan bir işlem olur; ve bayilik, birinin yazdığı bir özellik değil, modelin bedavaya yaptığı bir
şey haline gelir.

**İzolasyonu sen seçersin.** İş yükü başına `process`, `container`, `microvm`, `vm`; hepsi tek bir
sürücü arayüzünün arkasında. Her kademe, ne durdurduğu kadar ne durdur*ma*dığıyla da belgelenir,
çünkü izolasyon gibi görünüp izolasyon olmayan bir kademe, dürüstçe daha zayıf etiketlenmiş bir
kademeden daha kötüdür.

**Zorlamadığı şeyi iddia etmez.** Gösterilen her limit çekirdek tarafından uygulanır ve ölçülür.
Her durum gözlemlenir. Korpis bir şeyi göremiyorsa tahmin etmek yerine `unknown` der; veri
düşürdüğünde temiz görünen bir boşluk değil, işaretlenmiş bir boşluk bırakır.

**Büyümeyi vergilendirmez.** İş yükü başına, düğüm başına, örnek başına, hesap başına veya alan adı
başına hiçbir ücret yok; hiçbir ölçekte, asla. Sürüm ayrımı yok, kilitli özellik yok, ticari
kullanım kısıtı yok.

---

## Buradan başla

**[`00-overview.md`](./docs/00-overview.md)**: Korpis'in ne olduğu, bilerek ne *olmadığı*, on ilke,
yanlışlanabilir dört bahis ve on ikisi kanıttan türetilmiş, bağlayıcı on sekiz kural.

Sonrası, gerçekten neyi merak ettiğine bağlı:

| Soru | Doküman |
|---|---|
| Model tutarlı mı? | [`01-model.md`](./docs/01-model.md) · [`02-architecture.md`](./docs/02-architecture.md) · [`03-state.md`](./docs/03-state.md) |
| Güvenli mi? | [`17-security.md`](./docs/17-security.md) · [`04-runtimes.md`](./docs/04-runtimes.md) · [`08-identity.md`](./docs/08-identity.md) |
| **Gerçekle temasa dayanıyor mu?** | [`23-walkthroughs.md`](./docs/23-walkthroughs.md): uçtan uca izlenen otuz sekiz senaryo, buldukları kusurlar ve iki dış inceleme |
| Çalıştırabilir miyim? | [`18-operations.md`](./docs/18-operations.md) |
| Üstüne bir şey inşa edebilir miyim? | [`10-api.md`](./docs/10-api.md) · [`16-extensions.md`](./docs/16-extensions.md) · [`09-recipes.md`](./docs/09-recipes.md) |
| Önce ne yapılacak? | [`20-roadmap.md`](./docs/20-roadmap.md) |
| Kararları kim veriyor? | [`19-governance.md`](./docs/19-governance.md) |
| Bunların sebebi ne? | [`docs/research/evidence.md`](./docs/research/evidence.md) |

Tam harita genel bakışın §7'sinde.

```
README.md CONTRIBUTING.md SECURITY.md LICENSE*  giriş noktaları, inceleme, bildirim, lisans
CLAUDE.md                                       bu dokümanları düzenleyen herkesin uyduğu kurallar
docs/00-overview.md .. docs/23-walkthroughs.md  spesifikasyon, bağımlılık sırasına göre numaralı
docs/research/evidence.md                       her sapmanın dayandığı kaynaklar
crates/                                         uygulama, bir Rust workspace'i
ARCHITECTURE.md                                 kodun katmanları ve gerekçesi
```

**Açık sorular eksiklik değil, asıl mesele.** Dokümanlara dağılmış elli yedi açık soru var; her
biri onu karara bağlayacak dokümanın içine yazıldı. Üzerinde adı olan ertelenmiş bir karar
dürüsttür. Beyan edilmemiş olan ise birinin sonradan içine düşeceği bir tuzaktır.

---

## Bu nasıl yazıldı

**Tek kişi tarafından, Claude ile.** Bunu birinin fark etmesine bırakmak yerine açıkça söylemek
daha doğru.

Hiç kimse bir spesifikasyona onu kimin ya da neyin ürettiğine bakarak güvenmemeli. O yüzden,
okumaya değer olması için fiilen ne yapıldığı:

- **Her sapma alıntılanabilir bir şeyden türetildi ya da bir karar olduğunu söylüyor.** Genel
  bakışın §6'sındaki K-1 ile K-12 arasındaki kuralların her biri yayımlanmış bir güvenlik
  bildirimine, belirli bir issue'ya veya belgelenmiş ürün davranışına dayanır; hepsi
  [`docs/research/evidence.md`](./docs/research/evidence.md) içinde güven etiketleriyle toplandı.
  K-13 ile K-18 politika ve tasarım kararlarıdır ve aynı dosya her birinin neye dayandığını yazar.
  SEO içeriği çıkan kaynaklar öyle işaretlendi ve hiçbir şeye dayanak yapılmadı.
- **Tasarıma kasıtlı olarak saldırıldı.** [`23-walkthroughs.md`](./docs/23-walkthroughs.md) on dört
  somut senaryoyu dokundukları her katman boyunca izler ve özellikle hiçbir dokümanın topu
  tutmadığı adımı arar. Yirmi üç kusur buldu: bir kota yarışı, geri yüklemeden sonra dirilebilen
  iptal edilmiş yetki, her gözlem sağlıklı derken trafiği kesebilen bir kenar ve kaldırıldığında
  hiçbir şeye yayılmayan bir sohbet rolü de bunların arasında. Hepsi düzeltildi ve inceleme
  dokümanı onları sessizce yamamak yerine kayda geçirdi. Uygulama inşa edildikçe ondan yazılan
  yirmi dört izleme daha başka kusurlar buldu ve her biri aynı şekilde kayda geçirildi.
- **İkinci bir okuma dikişlere değil tanımlara saldırdı.** Farklı bir modelle yapılan bağımsız bir
  inceleme, her dokümanın kendi cebrini hedef aldı: bu koşulu ne, hangi bilgiyle değerlendiriyor ve
  değerlendirebilir mi. On dokuz şey buldu, on yedisi gerçekti; aralarında kimlik dokümanının
  gösterdiği ama hiçbir doğrulayıcının uygulayamayacağı bir limit de vardı. Hepsi yamandı ve
  [`23-walkthroughs.md`](./docs/23-walkthroughs.md) §16 hem her birini hem de denetimden geçemeyen
  iki iddiayı kayda geçirdi.
- **Sonra her şey yeniden okundu, hem kendisine hem kaynaklarına karşı.** İkinci bir inceleme her
  dokümanı diğer her dokümana karşı okudu ve başka bir projeye dair her iddiayı birincil kaynağıyla
  karşılaştırdı. Yetmiş dokuz şey buldu: önceki düzeltmelerin yarım bıraktığı protokol mekanikleri,
  dışarı arama tasarımıyla çelişen bir veri yolu ve kanıt dosyasının kendisinde dokuz yanlış atıf;
  aralarında yanıtsız denilen ama aynı gün yanıtlanmış bir issue da var.
  [`23-walkthroughs.md`](./docs/23-walkthroughs.md) §17 her birini ve nerede düzeltildiğini
  kaydeder.
- **Düzeltmeler görünür bırakıldı.** [`01-model.md`](./docs/01-model.md) §2, bu depodaki daha eski
  bir önerinin neden yanlış olduğunun kaydıdır. Hiçbir şey baştan belliymiş gibi sunulmuyor.

Muhakemeler, öncelikler ve hatalar yazara ait. Burada bir şey yanlışsa, kendi gerekçeleriyle
yanlıştır. Bir issue aç ve söyle.

---

## Katkı

Şu anda yapılabilecek en yararlı şey **tartışmak**. Kimsenin itiraz etmediği bir spesifikasyon
incelenmemiş demektir.

Özellikle aranan:

- Muhakemenin tutmadığı bir yer veya hiç dile getirilmemiş bir varsayım
- İncelemelerin kaçırdığı operasyonel bir arıza biçimi
- [`docs/research/evidence.md`](./docs/research/evidence.md) içinde yanlış veya haksız olan
  herhangi bir şey. Burada doğruluk, haklı olmaktan daha önemli
- Bu tür yazılımı ölçekte işletme deneyimi; hiçbir tasarım bunun yerini tutmaz

Bkz. [`CONTRIBUTING.md`](./CONTRIBUTING.md). Güvenlik politikası: [`SECURITY.md`](./SECURITY.md).

---

## Lisans

| Katman | Lisans |
|---|---|
| Protokol tanımları, istemci SDK'ları, tarif formatı, ajan protokolü | **Apache-2.0** |
| Kontrol düzlemi, düğüm ajanı, aktarıcı (relay), web / CLI / sohbet istemcileri | **AGPL-3.0-or-later** |
| Bu dokümanlar | **CC BY 4.0** |
| Tarifler, eklentiler, temalar | Yazarın seçimi, tescilli dahil |

Açıkça söylemek gerekirse, çünkü AGPL sıkça kullanım kısıtı sanılıyor: **ticari olarak çalıştır,
üzerinden kapasite sat, istediğin fiyatı iste, ona karşı kapalı eklentiler yaz.** Değiştirmeden,
müşterilerin için çalıştırman hiçbir yükümlülük doğurmaz. Değiştirip değiştirilmiş sürümü ağ
üzerinden sunarsan, kullanıcılarına kendi kaynak kodunu sunarsın; kopya dağıtırsan kaynak da
onlarla gider, ki değiştirilmemiş bir derleme bunu zaten sağlar.

Katkılar Developer Certificate of Origin altında alınır; CLA ve telif devri yoktur, dolayısıyla
kimse bir katkıcının kodunu onun rızası olmadan yeniden lisanslayamaz. Lisans tek başına
open-core'a dönüşü engellemez; bunu projenin politikası (P10, K-13) engeller ve
[`19-governance.md`](./docs/19-governance.md) §2 hangisinin hangisi olduğunu söyler.

---

> **Dokümanlar İngilizce.** Bu README bir giriş noktası, spesifikasyonun kendisi değil. `docs/`
> altındaki yirmi dört doküman İngilizce yazıldı ve öyle kalacak: ajan protokolü, kural adları ve
> model isimleri tek bir dilde tanımlanmak zorunda, yoksa iki çeviri arasında sessizce ayrışırlar.
> Bkz. [`CLAUDE.md`](./CLAUDE.md).
