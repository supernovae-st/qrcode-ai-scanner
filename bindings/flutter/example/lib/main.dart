import 'package:flutter/material.dart';
import 'package:flutter/services.dart' show rootBundle;
import 'package:qrcode_ai_scanner/qrcode_ai_scanner.dart';

Future<void> main() async {
  WidgetsFlutterBinding.ensureInitialized();
  await QrcodeAiScanner.init(); // RustLib.init() once, before any scan
  runApp(const DemoApp());
}

class DemoApp extends StatefulWidget {
  const DemoApp({super.key});

  @override
  State<DemoApp> createState() => _DemoAppState();
}

class _DemoAppState extends State<DemoApp> {
  String _out = 'tap “scan the sample QR”';
  bool _busy = false;

  Future<void> _scan() async {
    setState(() => _busy = true);
    try {
      final bytes =
          (await rootBundle.load('assets/sample_qr.png')).buffer.asUint8List();
      final report = await QrcodeAiScanner.scan(bytes, profile: 'full');
      setState(() {
        _out = report.detections.isEmpty
            ? 'no QR found'
            : 'text:  ${report.detections.first.content.text}\n'
                'grade: ${report.score?.grade.name ?? "n/a"} '
                '(${report.score?.value ?? "-"})';
      });
    } catch (e) {
      setState(() => _out = 'error: $e');
    } finally {
      setState(() => _busy = false);
    }
  }

  @override
  Widget build(BuildContext context) {
    return MaterialApp(
      home: Scaffold(
        appBar: AppBar(title: const Text('qrcode_ai_scanner demo')),
        body: Center(
          child: Padding(
            padding: const EdgeInsets.all(24),
            child: Column(
              mainAxisAlignment: MainAxisAlignment.center,
              children: [
                Image.asset('assets/sample_qr.png', width: 160, height: 160),
                const SizedBox(height: 24),
                FilledButton(
                  onPressed: _busy ? null : _scan,
                  child: const Text('scan the sample QR'),
                ),
                const SizedBox(height: 24),
                Text(
                  _out,
                  textAlign: TextAlign.center,
                  style: const TextStyle(fontSize: 16),
                ),
              ],
            ),
          ),
        ),
      ),
    );
  }
}
