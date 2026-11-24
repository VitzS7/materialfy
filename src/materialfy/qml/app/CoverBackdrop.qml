import QtQuick
import Md3

// Размытая обложка во весь элемент: фон полноэкранного плеера и атмосферная подложка окна.
// Размытие — увеличение крошечной копии обложки (сторона _side текселей) шейдером backdrop:
// ни свёртки, ни полноэкранной текстуры; сила размытия blur (0..1) — размер копии.
// mode — характер размытия:
//   gauss    — ровное, как гауссово: гладкие переходы без сетки и полос;
//   glass    — матовое стекло: обложка угадывается, поверх — мелкое зерно;
//   palette  — без обложки: плавный градиент из цветов темы (они и так взяты из обложки);
//   blobs    — крупные цветовые пятна (линейная интерполяция).
// flow (0..1) — «плавание»: картинка на месте, переливается только размытие.
Item {
    id: root

    property string source
    property string mode: "gauss"
    property real blur: 1
    property real flow: 0.5
    property bool running: visible

    readonly property bool _palette: mode === "palette"
    readonly property bool ready: _palette || img.status === Image.Ready
    readonly property real _blur: _palette ? 1 : Math.max(0, Math.min(1, blur))
    readonly property bool _flowing: flow > 0 && _blur > 0.2   // чёткую обложку «переливать» нечего
    property real _t: 0

    readonly property int _side: {
        const b = _blur
        if (mode === "blobs")
            return Math.round(12 + 788 * Math.pow(1 - b, 3))
        const min = mode === "glass" ? 14 : 6
        return Math.round(min * Math.pow(800 / min, Math.pow(1 - b, 1.5)))
    }

    Image {
        id: img
        width: root._side
        height: root._side
        // для слабого размытия лучше крупная обложка (coverLarge) — её передаёт владелец
        source: root._palette ? "" : root.source
        sourceSize: Qt.size(root._side, root._side)
        asynchronous: true
        smooth: true
        visible: false
        layer.enabled: true
        layer.smooth: true
        layer.textureSize: Qt.size(root._side, root._side)
    }

    // Палитра: сетка 4×3 из цветов темы — шейдер растягивает её так же, как копию обложки
    Grid {
        id: swatches
        columns: 4
        width: 4
        height: 3
        visible: false
        layer.enabled: root._palette
        layer.smooth: true
        layer.textureSize: Qt.size(4, 3)
        Repeater {
            model: root._palette ? [
                "primaryPaletteKeyColor", "secondaryContainer", "tertiaryContainer", "tertiaryPaletteKeyColor",
                "primaryContainer", "primaryPaletteKeyColor", "secondaryPaletteKeyColor", "tertiaryContainer",
                "tertiaryPaletteKeyColor", "primaryContainer", "primaryPaletteKeyColor", "secondaryContainer"
            ] : []
            Rectangle {
                required property string modelData
                width: 1
                height: 1
                color: Theme[modelData]
            }
        }
    }

    // Статичный фон без «плавания»: та же крошечная копия (img._side px), растянутая билинейно —
    // почти тот же мягкий градиент, что в шейдере, но без полноэкранного прохода в каждом кадре.
    // Декод offscreen (sourceSize + asynchronous): полноразмерная обложка в GUI-потоке — фриз.
    Image {
        id: staticImg
        anchors.fill: parent
        source: img.source
        sourceSize: Qt.size(root._side, root._side)
        asynchronous: true
        visible: !root._palette && staticImg.status === Image.Ready && root.ready && !root._flowing
        fillMode: Image.PreserveAspectCrop
        smooth: true
    }

    ShaderEffect {
        anchors.fill: parent
        // Без «плавания» шейдер не нужен: он каждый кадр семплирует и смешивает всю площадь окна.
        // Статичная копия обложки (та же кубическая интерполяция блюра) дешевле на порядки.
        visible: root.ready && (root._palette || root._flowing)
        property var source: root._palette ? swatches : img
        property real time: root._t
        property real amp: root._flowing ? 0.08 * root._blur : 0
        property real inset: root._flowing ? 0.06 : 0
        property real cubic: root.mode === "blobs" ? 0 : 1
        property real saturation: root._palette ? 1 : 1.1 + 0.4 * (1 - root._blur)
        property real grain: root.mode === "glass" ? 0.05 : 0
        property size texSize: root._palette ? Qt.size(4, 3) : Qt.size(root._side, root._side)
        property point crop: root._palette ? Qt.point(1, 1)
                           : width >= height ? Qt.point(1, height / Math.max(1, width))
                                             : Qt.point(width / Math.max(1, height), 1)
        fragmentShader: Qt.resolvedUrl("shaders/backdrop.frag.qsb")
    }

    TickAnimation {
        running: root.running && root._flowing && root.ready && !Theme.quiet
        // обновляем шейдер только когда _t реально сдвинулся: движение еле заметное,
        // а каждый кадр униформы — перерисовка всей площади окна; так — ~8 Гц вместо 30
        property real _acc: 0
        onTriggered: {
            _acc += frameTime * (0.15 + 0.85 * Math.min(1, root.flow))
            if (_acc >= 0.002) {
                root._t += _acc
                _acc = 0
            }
        }
    }
}
