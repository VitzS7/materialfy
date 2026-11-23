// Мост Qt (cpp/mobile.cpp) ↔ Android: состояние плеера → PlaybackService, системные панели, «назад», вибрация.
// Команды из уведомления и MediaSession уходят в нативный код через onMediaCommand.
package org.materialfy;

import android.app.Activity;
import android.content.Context;
import android.content.Intent;
import android.net.wifi.WifiManager;
import android.os.Build;
import android.view.HapticFeedbackConstants;
import android.view.View;
import android.view.WindowInsetsController;

import java.lang.ref.WeakReference;

public final class MobileBridge
{
    private static WeakReference<Activity> activity = new WeakReference<>(null);
    // Spotify Connect: колонки в сети ищутся через mDNS, а без этой блокировки Android отбрасывает multicast
    private static WifiManager.MulticastLock multicast;

    private MobileBridge() {}

    // Регистрирует cpp/mobile.cpp (register_natives)
    public static native void onMediaCommand(String command, long arg);

    public static float fontScale(Context context)
    {
        return context.getResources().getConfiguration().fontScale;
    }

    public static float textSize(Context context, float sp)
    {
        android.util.DisplayMetrics metrics = context.getResources().getDisplayMetrics();
        return metrics.density > 0
            ? android.util.TypedValue.applyDimension(android.util.TypedValue.COMPLEX_UNIT_SP, sp, metrics) / metrics.density
            : sp;
    }

    // Из Java-кода: команда в приложение. Нативная часть грузится вместе с активностью; если плеер системы
    // нажал кнопку раньше (процесс поднят ради MediaBrowser), команды некому принять — не падаем
    static void send(String command, long arg)
    {
        try {
            onMediaCommand(command, arg);
        } catch (UnsatisfiedLinkError e) {
            android.util.Log.w("materialfy", "media command before the app started: " + command);
        }
    }

    static void attach(Activity a)
    {
        activity = new WeakReference<>(a);
        if (multicast == null) {
            WifiManager wifi = (WifiManager) a.getApplicationContext().getSystemService(Context.WIFI_SERVICE);
            if (wifi != null) {
                multicast = wifi.createMulticastLock("materialfy:mdns");
                multicast.setReferenceCounted(false);
                multicast.acquire();
            }
        }
    }

    static void detach(Activity a)
    {
        if (activity.get() == a)
            activity = new WeakReference<>(null);
    }

    static Activity activityOf(Context context)
    {
        if (context instanceof Activity)
            return (Activity) context;
        return activity.get();
    }

    // Вызывается из потока Qt: сервис сам переносит работу в главный поток
    public static void updateMedia(Context context, String title, String artist, String album, String coverUrl,
                                   long durationMs, long positionMs, boolean playing, boolean hasTrack,
                                   boolean canPrevious, boolean canNext, boolean local, boolean clockRunning)
    {
        PlaybackService.State state = new PlaybackService.State();
        state.title = title;
        state.artist = artist;
        state.album = album;
        state.coverUrl = coverUrl;
        state.durationMs = durationMs;
        state.positionMs = positionMs;
        state.playing = playing;
        state.clockRunning = clockRunning;
        state.hasTrack = hasTrack;
        state.canPrevious = canPrevious;
        state.canNext = canNext;
        state.local = local;
        PlaybackService.publish(context.getApplicationContext(), state);
    }

    // Сердце в карточке плеера системы: текущий трек в любимых или нет
    public static void setMediaLiked(boolean liked)
    {
        PlaybackService.setLiked(liked);
    }

    // lightBars=true — тёмные значки статус-бара и навигации (светлая тема интерфейса)
    public static void setSystemBars(Context context, boolean lightBars)
    {
        Activity a = activityOf(context);
        if (a == null)
            return;
        View decor = a.getWindow().getDecorView();
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
            WindowInsetsController controller = a.getWindow().getInsetsController();
            if (controller == null)
                return;
            int mask = WindowInsetsController.APPEARANCE_LIGHT_STATUS_BARS
                     | WindowInsetsController.APPEARANCE_LIGHT_NAVIGATION_BARS;
            controller.setSystemBarsAppearance(lightBars ? mask : 0, mask);
        } else {
            int flags = decor.getSystemUiVisibility();
            int light = View.SYSTEM_UI_FLAG_LIGHT_STATUS_BAR | View.SYSTEM_UI_FLAG_LIGHT_NAVIGATION_BAR;
            decor.setSystemUiVisibility(lightBars ? (flags | light) : (flags & ~light));
        }
    }

    // Плеер в острове системы (IslandNotification)
    public static boolean islandSupported() { return IslandNotification.supported(); }

    public static boolean islandEnabled(Context context) { return IslandNotification.enabled(context); }

    public static void setIslandEnabled(Context context, boolean on)
    {
        IslandNotification.setEnabled(context, on);
        PlaybackService.refresh();
    }

    // Настройки уведомления плеера: "mediaColorized" (в цвет обложки), "mediaLikeButton" (сердце)
    public static boolean mediaOption(Context context, String key) { return IslandNotification.option(context, key); }

    public static void setMediaOption(Context context, String key, boolean on)
    {
        IslandNotification.setOption(context, key, on);
        PlaybackService.refresh();
    }

    // Выбор из нескольких вариантов: "mediaBackground" ("cover" / "canvas")
    public static String mediaChoice(Context context, String key) { return IslandNotification.choice(context, key); }

    public static void setMediaChoice(Context context, String key, String value)
    {
        IslandNotification.setChoice(context, key, value);
        PlaybackService.refresh();
    }

    // Ссылка на Canvas текущего трека для фона уведомления (пустая строка: нет)
    public static void setMediaCanvas(String url)
    {
        PlaybackService.setCanvas(url);
    }

    // Что мешает музыке играть в фоне (через запятую): "battery" — оптимизация батареи, "notifications" — уведомления
    // выключены, "promoted" — Live Update (остров) запрещён, "autostart" — у оболочки есть свой список автозапуска
    // (его состояние прочитать нельзя, поэтому пункт показывается всегда, пока пользователь его не отметил)
    public static String backgroundIssues(Context context)
    {
        java.util.List<String> issues = new java.util.ArrayList<>();
        android.os.PowerManager pm = (android.os.PowerManager) context.getSystemService(Context.POWER_SERVICE);
        if (pm != null && !pm.isIgnoringBatteryOptimizations(context.getPackageName()))
            issues.add("battery");
        android.app.NotificationManager nm =
            (android.app.NotificationManager) context.getSystemService(Context.NOTIFICATION_SERVICE);
        if (nm != null && !nm.areNotificationsEnabled())
            issues.add("notifications");
        if (nm != null && android.os.Build.VERSION.SDK_INT >= 36 && IslandNotification.enabled(context)
            && !nm.canPostPromotedNotifications())
            issues.add("promoted");
        if (autostartIntents().length > 0)
            issues.add("autostart");
        return String.join(",", issues);
    }

    // Экраны автозапуска оболочек, которые убивают фоновые приложения сверх оптимизации батареи
    private static Intent[] autostartIntents()
    {
        String maker = (android.os.Build.MANUFACTURER + " " + android.os.Build.BRAND).toLowerCase(java.util.Locale.ROOT);
        if (maker.contains("xiaomi") || maker.contains("redmi") || maker.contains("poco"))
            return new Intent[] { new Intent().setClassName("com.miui.securitycenter",
                                                            "com.miui.permcenter.autostart.AutoStartManagementActivity") };
        if (maker.contains("oppo") || maker.contains("realme") || maker.contains("oneplus"))
            return new Intent[] {
                new Intent().setClassName("com.coloros.safecenter",
                                          "com.coloros.safecenter.permission.startup.StartupAppListActivity"),
                new Intent().setClassName("com.oplus.battery", "com.oplus.powermanager.fuelgaue.PowerUsageModelActivity"),
                new Intent().setClassName("com.oneplus.security", "com.oneplus.security.chainlaunch.view.ChainLaunchAppListActivity")
            };
        if (maker.contains("vivo") || maker.contains("iqoo"))
            return new Intent[] {
                new Intent().setClassName("com.vivo.permissionmanager", "com.vivo.permissionmanager.activity.BgStartUpManagerActivity"),
                new Intent().setClassName("com.iqoo.secure", "com.iqoo.secure.ui.phoneoptimize.AddWhiteListActivity")
            };
        if (maker.contains("huawei") || maker.contains("honor"))
            return new Intent[] { new Intent().setClassName("com.huawei.systemmanager",
                                                            "com.huawei.systemmanager.startupmgr.ui.StartupNormalAppListActivity") };
        return new Intent[0];
    }

    public static void openBackgroundSetting(Context context, String what)
    {
        String pkg = context.getPackageName();
        Intent main = null;
        switch (what) {
        case "battery":
            main = new Intent(android.provider.Settings.ACTION_REQUEST_IGNORE_BATTERY_OPTIMIZATIONS,
                              android.net.Uri.parse("package:" + pkg));
            break;
        case "notifications":
            main = new Intent(android.provider.Settings.ACTION_APP_NOTIFICATION_SETTINGS)
                       .putExtra(android.provider.Settings.EXTRA_APP_PACKAGE, pkg);
            break;
        case "promoted":
            if (android.os.Build.VERSION.SDK_INT >= 36)
                main = new Intent(android.provider.Settings.ACTION_APP_NOTIFICATION_PROMOTION_SETTINGS)
                           .putExtra(android.provider.Settings.EXTRA_APP_PACKAGE, pkg);
            break;
        case "autostart":
            for (Intent intent : autostartIntents()) {
                if (start(context, intent))
                    return;
            }
            break;
        default:
            break;
        }
        if (main != null && start(context, main))
            return;
        // запасной путь: страница приложения в настройках, оттуда доступны батарея и уведомления
        start(context, new Intent(android.provider.Settings.ACTION_APPLICATION_DETAILS_SETTINGS,
                                  android.net.Uri.parse("package:" + pkg)));
    }

    private static boolean start(Context context, Intent intent)
    {
        try {
            context.startActivity(intent.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK));
            return true;
        } catch (RuntimeException e) {
            // экрана нет в этой прошивке или он закрыт для сторонних приложений
            return false;
        }
    }

    // Вход в Spotify ждёт ответа браузера (см. SignInService)
    public static void setBrowserWait(Context context, boolean waiting)
    {
        SignInService.setWaiting(context, waiting);
    }

    public static void moveTaskToBack(Context context)
    {
        Activity a = activityOf(context);
        if (a != null)
            a.moveTaskToBack(true);
    }

    public static void bringToFront(Context context)
    {
        Intent intent = new Intent(context, MaterialfyActivity.class);
        intent.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK | Intent.FLAG_ACTIVITY_REORDER_TO_FRONT
                        | Intent.FLAG_ACTIVITY_SINGLE_TOP);
        try {
            context.startActivity(intent);
        } catch (RuntimeException e) {
            // фоновый запуск активности запрещён — пользователь вернётся сам
        }
    }

    // «Поделиться журналом» (настройки): текст в системное окно выбора — мессенджер, почта, «Копировать»
    public static void shareText(Context context, String title, String text)
    {
        Intent send = new Intent(Intent.ACTION_SEND)
                          .setType("text/plain")
                          .putExtra(Intent.EXTRA_SUBJECT, title)
                          .putExtra(Intent.EXTRA_TEXT, text);
        Intent chooser = Intent.createChooser(send, title).addFlags(Intent.FLAG_ACTIVITY_NEW_TASK);
        try {
            context.startActivity(chooser);
        } catch (RuntimeException e) {
            android.util.Log.w("materialfy", "share: " + e);
        }
    }

    public static void haptic(Context context)
    {
        Activity a = activityOf(context);
        if (a != null)
            a.getWindow().getDecorView().performHapticFeedback(HapticFeedbackConstants.CONTEXT_CLICK);
    }
}
