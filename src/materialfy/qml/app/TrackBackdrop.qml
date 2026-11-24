import QtQuick
import Md3

// Фон из обложки играющего трека (CoverBackdrop) с двойным буфером: атмосферная подложка окна и фон
// полноэкранного плеера. Новая обложка заранее грузится в скрытый слой, а меняется в тот же момент,
// что и цвета приложения (Theme.schemeApplied): под шторкой — мгновенно, и шторка открывает новый
// фон вместе с новыми цветами; при плавной смене схемы — проявляясь поверх старого.
// Если тема от обложки не зависит (followTheme: false) — сразу, как загрузится; если схема
// не изменилась — сам, чуть погодя.
Item {
    id: root

    property string source
    property string mode: "gauss"
    property real blur: 1
    property real flow: 0.5
    property bool running: true
    property bool followTheme: true

    property int _front: 0
    property bool _waiting: false          // пора меняться, а новая обложка ещё грузится
    readonly property var _layers: [a, b]
    readonly property var _back: _layers[1 - _front]

    // Полупрозрачный фон (атмосферная подложка окна): opacity в Qt Quick ложится на каждый слой отдельно,
    // и пока новая обложка проявляется поверх старой, они складываются — фон светлеет, а в конце перехода
    // резко темнеет. На время перехода — общий слой, прозрачность на двоих одна
    layer.enabled: fade.running && opacity < 1

    onSourceChanged: {
        _settle()
        _back.source = source
        _waiting = false
        if (!followTheme || _layers[_front].source === "")   // ждать нечего / первая обложка
            _swapOrWait(true)
        else
            fallback.restart()
    }

    function _settle() {
        fade.stop()
        _layers[_front].opacity = 1
        _back.opacity = 0
    }

    function _swapOrWait(animated) {
        const back = _back, front = _layers[_front]
        if (back.source !== source)
            return                          // нечего менять
        if (!back.ready) {
            _waiting = true
            return
        }
        _settle()
        fallback.stop()
        _waiting = false
        back.z = 1
        front.z = 0
        _front = 1 - _front
        if (animated) {                     // новый проявляется поверх старого — без провала в цвет темы
            front.opacity = 1
            fade.target = back
            fade.start()
        } else {
            _settle()
        }
    }

    NumberAnimation {
        id: fade
        property: "opacity"
        from: 0
        to: 1
        duration: 700
        easing.type: Easing.InOutCubic
        onFinished: root._settle()
    }

    Connections {
        target: Theme
        function onSchemeApplied(instant) { root._swapOrWait(!instant) }
    }
    Connections {
        target: root._back
        function onReadyChanged() {
            if (root._back.ready && root._waiting)
                root._swapOrWait(true)
        }
    }
    // Схема не поменялась — меняем сами
    Timer {
        id: fallback
        interval: 2500
        onTriggered: root._swapOrWait(true)
    }

    component Layer: CoverBackdrop {
        anchors.fill: parent
        mode: root.mode
        blur: root.blur
        flow: root.flow
        visible: opacity > 0 && ready
        running: root.running && visible
    }

    Layer { id: a }
    Layer { id: b; opacity: 0 }
}
