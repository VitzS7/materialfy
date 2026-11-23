// Плеер в «острове» системы: отдельное уведомление рядом с медиа-уведомлением (MediaStyle повышать нельзя).
//  - Android 16+: Live Update (запрос promoted ongoing + ProgressStyle). Его показывают чип статус-бара (Pixel),
//    Now Bar (Samsung One UI 8+), Super Island (HyperOS 3.1+), Fluid Cloud (ColorOS 16+) и другие оболочки,
//    которые выводят Live Updates.
//  - vivo OriginOS: «атомный остров» читает extras notification.superx.* (сцена MUSIC_PLAYBACK) — так делают
//    сторонние плееры (MeloX). Музыкальную карточку острова vivo открывает не всем приложениям: если прошивка
//    не пускает, остаётся обычное уведомление плеера.
// Выбирается в Настройки → Воспроизведение («Вид острова»): Live Update или только карточка медиа (по умолчанию).
package org.materialfy;

import android.app.Notification;
import android.app.NotificationChannel;
import android.app.NotificationManager;
import android.app.PendingIntent;
import android.content.Context;
import android.content.Intent;
import android.graphics.Bitmap;
import android.graphics.drawable.Icon;
import android.os.Build;
import android.os.Bundle;
import android.os.Handler;
import android.os.Looper;
import android.os.SystemClock;
import android.service.notification.StatusBarNotification;

import java.util.Locale;

final class IslandNotification
{
    private static final String CHANNEL = "island";
    static final int NOTIFICATION_ID = 3;
    private static final String PREFS = "materialfy";
    // "live": Live Update с полоской; "media": только карточка плеера системы. Кнопки Live Update система рисует
    // текстом (иконки действий в обычном шаблоне не показываются, а свою разметку повышенному уведомлению нельзя),
    // поэтому по умолчанию остров это карточка медиа с иконками
    private static final String KEY_STYLE = "islandStyle";
    // Настройки → Воспроизведение: уведомление плеера в цвет обложки и сердце в карточке плеера (по умолчанию да)
    static final String KEY_COLORIZE = "mediaColorized";
    static final String KEY_LIKE = "mediaLikeButton";
    // vivo отверг остров на этой прошивке и версии приложения (см. checkVivoVerdict)
    private static final String KEY_VIVO_REJECTED = "islandVivoRejected";
    // vivo: прогресс острова обновляется не чаще, чем раз в 10 с (так советуют плееры, которые это делают)
    private static final long SLOW_INTERVAL_MS = 10_000L;
    // остальные оболочки: полоска идёт раз в секунду, пока экран включён (на выключенном — реже, см. tickMs)
    private static final long LIVE_INTERVAL_MS = 1_000L;

    private String lastKey = "";
    private boolean lastClockRunning;
    private long lastAt;
    private long lastPosition;
    private long lastDuration;
    private boolean lastPlaying;

    static boolean isVivo()
    {
        String m = Build.MANUFACTURER.toLowerCase(Locale.ROOT);
        String b = Build.BRAND.toLowerCase(Locale.ROOT);
        return m.contains("vivo") || b.contains("vivo") || b.contains("iqoo");
    }

    // как часто обновлять полоску: до этого она отставала на целых 10 с от настоящей позиции
    static long tickMs(Context context)
    {
        if (isVivo())
            return SLOW_INTERVAL_MS;
        android.os.PowerManager pm = (android.os.PowerManager) context.getSystemService(Context.POWER_SERVICE);
        // в режиме энергосбережения реже: каждое обновление будит процесс и системный интерфейс
        return pm != null && pm.isInteractive() && !pm.isPowerSaveMode() ? LIVE_INTERVAL_MS : SLOW_INTERVAL_MS;
    }

    // Цвет карточки по обложке: средний по насыщенным пикселям (как акцент плеера на ПК)
    private static Bitmap accentOf;
    private static int accentValue;

    static synchronized int accent(Bitmap cover, int fallback)
    {
        if (cover == null)
            return fallback;
        // тик раз в секунду: на ту же обложку цвет не пересчитываем
        if (cover == accentOf)
            return accentValue;
        Bitmap small = Bitmap.createScaledBitmap(cover, 16, 16, true);
        float[] hsv = new float[3];
        double r = 0, g = 0, b = 0, weight = 0;
        for (int y = 0; y < 16; y++) {
            for (int x = 0; x < 16; x++) {
                int c = small.getPixel(x, y);
                android.graphics.Color.colorToHSV(c, hsv);
                // серые и очень тёмные/светлые пиксели не дают цвета
                double w = hsv[1] * Math.min(hsv[2], 1f - 0.5f * Math.max(0f, hsv[2] - 0.8f)) + 0.01;
                r += android.graphics.Color.red(c) * w;
                g += android.graphics.Color.green(c) * w;
                b += android.graphics.Color.blue(c) * w;
                weight += w;
            }
        }
        if (small != cover)
            small.recycle();
        android.graphics.Color.colorToHSV(android.graphics.Color.rgb((int) (r / weight), (int) (g / weight),
                                                                     (int) (b / weight)), hsv);
        // яркость в середину, чтобы белый текст и кнопки читались на любой обложке
        hsv[1] = Math.max(hsv[1], 0.35f);
        hsv[2] = Math.max(0.35f, Math.min(hsv[2], 0.55f));
        accentOf = cover;
        accentValue = android.graphics.Color.HSVToColor(hsv);
        return accentValue;
    }

    // Есть куда показать: Live Updates (Android 16) или остров vivo
    static boolean supported()
    {
        return Build.VERSION.SDK_INT >= 36 || isVivo();
    }

    static boolean enabled(Context context)
    {
        return "live".equals(choice(context, KEY_STYLE));
    }

    static void setEnabled(Context context, boolean on)
    {
        setChoice(context, KEY_STYLE, on ? "live" : "media");
    }

    static String choice(Context context, String key)
    {
        return context.getSharedPreferences(PREFS, Context.MODE_PRIVATE).getString(key, "media");
    }

    static void setChoice(Context context, String key, String value)
    {
        context.getSharedPreferences(PREFS, Context.MODE_PRIVATE).edit().putString(key, value).apply();
    }

    static boolean option(Context context, String key)
    {
        return context.getSharedPreferences(PREFS, Context.MODE_PRIVATE).getBoolean(key, true);
    }

    static void setOption(Context context, String key, boolean on)
    {
        context.getSharedPreferences(PREFS, Context.MODE_PRIVATE).edit().putBoolean(key, on).apply();
    }

    void update(Context context, PlaybackService.State s, Bitmap cover, int coverAccent, Bitmap tracker, PendingIntent open,
                Notification.Action previous, Notification.Action playPause, Notification.Action next)
    {
        NotificationManager nm = (NotificationManager) context.getSystemService(Context.NOTIFICATION_SERVICE);
        // на паузе карточка остаётся (с кнопкой «играть»), убирается вместе с плеером
        if (!supported() || !enabled(context) || !s.hasTrack || vivoRejected(context)) {
            clear(nm);
            return;
        }
        String key = s.title + "\u0000" + s.artist;
        long now = SystemClock.elapsedRealtime();
        // тот же трек — не чаще интервала (между обновлениями время ведёт таймер чипа)
        // перемотку и пришедшую длительность показать сразу, иначе на vivo и при выключенном экране полоска стояла бы до 10 с
        long expected = lastPosition + (lastClockRunning ? now - lastAt : 0);
        boolean jumped = Math.abs(s.positionMs - expected) > 1500 || s.durationMs != lastDuration;
        if (key.equals(lastKey) && s.clockRunning == lastClockRunning && s.playing == lastPlaying && !jumped
            && now - lastAt < tickMs(context) - 50)
            return;
        boolean newTrack = !key.equals(lastKey);
        lastKey = key;
        lastClockRunning = s.clockRunning;
        lastPlaying = s.playing;
        lastAt = now;
        lastPosition = s.positionMs;
        lastDuration = s.durationMs;

        ensureChannel(context, nm);
        int progress = s.durationMs > 0 ? (int) Math.max(0, Math.min(1000, s.positionMs * 1000 / s.durationMs)) : 0;
        boolean colorize = option(context, KEY_COLORIZE);
        // без цвета обложки: акцент системы
        int accent = colorize ? coverAccent : Notification.COLOR_DEFAULT;
        long remaining = s.durationMs > 0 ? Math.max(0, s.durationMs - s.positionMs) : 0;
        Notification.Builder b = new Notification.Builder(context, CHANNEL)
            .setSmallIcon(R.drawable.ic_notification)
            .setContentTitle(s.title)
            .setContentText(s.artist)
            .setSubText(s.album)
            .setColor(accent)
            .setOngoing(true)
            .setOnlyAlertOnce(true)
            .setLocalOnly(true)
            .setCategory(Notification.CATEGORY_TRANSPORT)
            .setVisibility(Notification.VISIBILITY_PUBLIC)
            .setContentIntent(open)
            // время в чипе острова идёт само и показывает, сколько осталось до конца трека
            .setWhen(System.currentTimeMillis() + (s.durationMs > 0 ? remaining : -Math.max(0, s.positionMs)))
            .setShowWhen(s.clockRunning)
            .setUsesChronometer(s.clockRunning)
            .setChronometerCountDown(s.durationMs > 0);
        if (cover != null)
            b.setLargeIcon(cover);
        if (previous != null)
            b.addAction(previous);
        if (playPause != null)
            b.addAction(playPause);
        if (next != null)
            b.addAction(next);

        Bundle extras = new Bundle();
        if (Build.VERSION.SDK_INT >= 36) {
            // Live Update: система сама решает, повысить ли (пользователь может запретить в настройках)
            extras.putBoolean("android.requestPromotedOngoing", true);
            // styledByProgress выключать нельзя: тогда весь сегмент закрашен и полоска всегда полная
            Notification.ProgressStyle.Segment segment = new Notification.ProgressStyle.Segment(1000);
            if (colorize)
                segment.setColor(accent);
            Notification.ProgressStyle style = new Notification.ProgressStyle().setProgress(progress)
                .setProgressSegments(java.util.Collections.singletonList(segment));
            // ползунок как у слайдера приложения (вертикальная «таблетка» цвета акцента); без цвета обложки
            // остаётся системный
            if (tracker != null && colorize)
                style.setProgressTrackerIcon(Icon.createWithBitmap(tracker));
            b.setStyle(style);
            // пока идёт время, чип показывает таймер до конца; название — только когда таймера нет
            if (!s.clockRunning)
                b.setShortCriticalText(shortTitle(s.title));
        }
        if (isVivo()) {
            extras.putInt("notification.superx.operation", newTrack ? 0 : 1);
            extras.putBoolean("notification.superx.showNotify", true);
            extras.putInt("notification.superx.template", 2);
            extras.putString("notification.superx.scene", "MUSIC_PLAYBACK");
            if (open != null)
                extras.putParcelable("notification.superx.clickResp", open);
            Bundle base = new Bundle();
            base.putParcelable("notification.superx.baseInfos.icon",
                               cover != null ? Icon.createWithBitmap(cover)
                                             : Icon.createWithResource(context, R.drawable.ic_notification));
            base.putCharSequence("notification.superx.baseInfos.title", s.title);
            base.putCharSequence("notification.superx.baseInfos.content", s.artist);
            extras.putBundle("notification.superx.baseInfos", base);
            if (Build.VERSION.SDK_INT < 36)
                b.setProgress(1000, progress, false);
        }
        b.addExtras(extras);
        try {
            nm.notify(NOTIFICATION_ID, b.build());
        } catch (RuntimeException e) {
            // прошивка отвергла расширенное уведомление — остаётся обычное уведомление плеера
            return;
        }
        if (isVivo() && newTrack)
            new Handler(Looper.getMainLooper()).postDelayed(() -> checkVivoVerdict(context, nm), 1500);
    }

    // vivo (Funtouch / OriginOS) пускает в остров по superx только свои приложения (разрешение
    // ISLAND_NOTI_SUPPORT — подписью системы) и помечает чужое уведомление SUPERX_VALUE=false. Live Update vivo
    // тоже не выводит — карточка «острова» в шторке тогда лишь дублирует уведомление плеера: убрать её и больше
    // не ставить на этой прошивке и этой версии приложения
    private void checkVivoVerdict(Context context, NotificationManager nm)
    {
        try {
            for (StatusBarNotification sbn : nm.getActiveNotifications()) {
                if (sbn.getId() != NOTIFICATION_ID)
                    continue;
                Object verdict = sbn.getNotification().extras.get("SUPERX_VALUE");
                if (verdict != null && "false".equals(String.valueOf(verdict))) {
                    context.getSharedPreferences(PREFS, Context.MODE_PRIVATE).edit()
                        .putString(KEY_VIVO_REJECTED, verdictKey(context)).apply();
                    clear(nm);
                }
                return;
            }
        } catch (RuntimeException e) {
            // нет ответа — оставить как есть
        }
    }

    private static boolean vivoRejected(Context context)
    {
        if (!isVivo())
            return false;
        String saved = context.getSharedPreferences(PREFS, Context.MODE_PRIVATE).getString(KEY_VIVO_REJECTED, "");
        return saved.equals(verdictKey(context));
    }

    // прошивка + версия приложения: после обновления любого из них — проверить заново
    private static String verdictKey(Context context)
    {
        long version = 0;
        try {
            version = context.getPackageManager().getPackageInfo(context.getPackageName(), 0).getLongVersionCode();
        } catch (Exception e) {
            // без версии — по одной прошивке
        }
        return Build.FINGERPRINT + "|" + version;
    }

    void clear(NotificationManager nm)
    {
        // и без своего lastKey: уведомление могло остаться от прежнего процесса приложения
        lastKey = "";
        lastAt = 0;
        nm.cancel(NOTIFICATION_ID);
    }

    private static String shortTitle(String title)
    {
        String t = title == null ? "" : title.trim();
        return t.length() > 12 ? t.substring(0, 11) + "…" : t;
    }

    private static void ensureChannel(Context context, NotificationManager nm)
    {
        if (nm.getNotificationChannel(CHANNEL) != null)
            return;
        // DEFAULT, но без звука и вибрации: каналы MIN/LOW Live Updates не повышают
        NotificationChannel channel = new NotificationChannel(CHANNEL, context.getString(R.string.island_channel),
                                                              NotificationManager.IMPORTANCE_DEFAULT);
        channel.setSound(null, null);
        channel.enableVibration(false);
        channel.setShowBadge(false);
        channel.setLockscreenVisibility(Notification.VISIBILITY_PUBLIC);
        nm.createNotificationChannel(channel);
    }
}
