import QtQuick
import QtQuick.Effects
import Md3
import Materialfy.Core

// Превью стиля текста для настроек: те же пропорции, заливка, свечение, размытие и
// масштаб неактивных строк, что у LyricsView, а не статичная жирная строка.
Item {
    id: root

    property real baseFontSize: 20
    property string mode: Settings.lyricsMode
    readonly property bool spicy: mode === "spicy"
    readonly property bool karaoke: mode === "karaoke"
    readonly property real activeScale: Settings.lyricsActiveScale >= 1.0
        ? Settings.lyricsActiveScale
        : (spicy ? 1.12 : (karaoke ? 1.16 : 1.0))
    readonly property real idleScale: Settings.lyricsIdleScale > 0
        ? Math.min(activeScale, Settings.lyricsIdleScale)
        : (spicy ? 0.88 : (karaoke ? 0.90 : 0.94))
    readonly property real fontPx: baseFontSize * Settings.lyricsFontScale * activeScale
    readonly property real dimAlpha: Settings.lyricsDimOpacity ?? (spicy ? 0.34 : 0.35)
    readonly property int alignment: Settings.lyricsTextAlign === "center" ? Text.AlignHCenter
        : (Settings.lyricsTextAlign === "right" ? Text.AlignRight : Text.AlignLeft)
    readonly property real feather: Math.max(0.005, (spicy ? 0.40 : (karaoke ? 0.02 : 0.10)) * Settings.lyricsHighlight)
    property real sweep: 0

    implicitHeight: col.implicitHeight

    SequentialAnimation {
        running: true
        loops: Animation.Infinite
        NumberAnimation {
            target: root
            property: "sweep"
            from: -0.08
            to: 1.08
            duration: 2800
            easing.type: Easing.InOutQuad
        }
        PauseAnimation { duration: 650 }
    }

    Column {
        id: col
        width: parent.width
        spacing: Math.round(root.fontPx * 0.26 * Settings.lyricsLineSpacing)

        Text {
            width: parent.width
            visible: Settings.lyricsVisibleLines !== "one"
            horizontalAlignment: root.alignment
            text: "When the music starts to play"
            font.family: Theme.fontFamily
            font.pixelSize: root.fontPx * 0.72
            color: Theme.fgSurface
            opacity: root.dimAlpha
            scale: root.idleScale / Math.max(1.0, root.activeScale)
            transformOrigin: Item.Center
            layer.enabled: Settings.lyricsBlurAmount > 0.001
            layer.effect: MultiEffect {
                blurEnabled: true
                blurMax: 16
                blur: 0.30 * Settings.lyricsBlurAmount
                autoPaddingEnabled: true
            }
        }

        Text {
            id: active
            width: parent.width
            horizontalAlignment: root.alignment
            text: root.karaoke ? "EVERY RHYTHM GUIDES THE WAY" : "Every rhythm guides the way"
            font.family: Theme.fontFamily
            font.pixelSize: root.fontPx
            font.weight: Theme.variableFont ? Font.Normal : Font.Bold
            font.variableAxes: Theme.fontAxes(root.fontPx, 700)
            color: Theme.fgSurface
            layer.enabled: true
            layer.smooth: true
            layer.effect: ShaderEffect {
                property real feather: root.feather
                property real dimAlpha: root.dimAlpha
                readonly property real progress: -root.feather + root.sweep * (1 + 2 * root.feather)
                readonly property real glow: root.spicy
                    ? 0.90 * Settings.lyricsHighlight * Math.max(0, Math.sin(Math.PI * Math.max(0, Math.min(1, root.sweep))))
                    : (root.karaoke ? 0.25 * Settings.lyricsHighlight : 0)
                fragmentShader: Qt.resolvedUrl("shaders/wordfill.frag.qsb")
            }
        }

        Text {
            width: parent.width
            visible: Settings.lyricsVisibleLines === "all"
            horizontalAlignment: root.alignment
            text: "Singing through the night and day"
            font.family: Theme.fontFamily
            font.pixelSize: root.fontPx * 0.72
            color: Theme.fgSurface
            opacity: root.dimAlpha
            scale: root.idleScale / Math.max(1.0, root.activeScale)
            transformOrigin: Item.Center
            layer.enabled: Settings.lyricsBlurAmount > 0.001
            layer.effect: MultiEffect {
                blurEnabled: true
                blurMax: 16
                blur: 0.30 * Settings.lyricsBlurAmount
                autoPaddingEnabled: true
            }
        }
    }
}
