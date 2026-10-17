/* Native-library experiment: packet-copy remux and headless mpv playback. */
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#include <libavcodec/avcodec.h>
#include <libavformat/avformat.h>
#include <libavutil/error.h>
#include <mpv/client.h>

static void check(int status, const char *operation)
{
    if (status < 0) {
        char message[AV_ERROR_MAX_STRING_SIZE];
        av_strerror(status, message, sizeof(message));
        fprintf(stderr, "%s: %s\n", operation, message);
        exit(1);
    }
}

static void capabilities(void)
{
    const char *muxers[] = {"mp4", "mov", "hls", "mpegts", "matroska", "webvtt"};
    const char *decoders[] = {"h264", "hevc", "libdav1d", "vp9", "aac", "pgssub", "ass"};
    for (size_t i = 0; i < sizeof(muxers) / sizeof(*muxers); i++) {
        if (!av_guess_format(muxers[i], NULL, NULL)) {
            fprintf(stderr, "missing muxer: %s\n", muxers[i]);
            exit(1);
        }
    }
    for (size_t i = 0; i < sizeof(decoders) / sizeof(*decoders); i++) {
        if (!avcodec_find_decoder_by_name(decoders[i])) {
            fprintf(stderr, "missing decoder: %s\n", decoders[i]);
            exit(1);
        }
    }
    void *iterator = NULL;
    const AVCodec *codec;
    while ((codec = av_codec_iterate(&iterator))) {
        if (av_codec_is_encoder(codec) && strcmp(codec->name, "png") && strcmp(codec->name, "mjpeg")) {
            fprintf(stderr, "unexpected encoder: %s\n", codec->name);
            exit(1);
        }
    }
    printf("FFmpeg %s; libmpv API %lu; required capabilities present\n",
           av_version_info(), mpv_client_api_version());
}

static void playback(const char *source)
{
    mpv_handle *mpv = mpv_create();
    if (!mpv) exit(1);
    const char *options[][2] = {
        {"config", "no"}, {"terminal", "no"}, {"vo", "null"},
        {"ao", "null"}, {"hwdec", "no"}, {"untimed", "yes"},
    };
    for (size_t i = 0; i < sizeof(options) / sizeof(*options); i++) {
        if (mpv_set_option_string(mpv, options[i][0], options[i][1]) < 0) exit(1);
    }
    if (mpv_initialize(mpv) < 0) exit(1);
    const char *command[] = {"loadfile", source, NULL};
    if (mpv_command(mpv, command) < 0) exit(1);
    /* Bound the probe even if a decoder or a runtime dependency is broken. */
    for (int i = 0; i < 300; i++) {
        mpv_event *event = mpv_wait_event(mpv, 0.1);
        if (event->event_id == MPV_EVENT_END_FILE) {
            mpv_event_end_file *end = event->data;
            if (end->reason != MPV_END_FILE_REASON_EOF) {
                fprintf(stderr, "mpv playback: %s\n", mpv_error_string(end->error));
                exit(1);
            }
            mpv_terminate_destroy(mpv);
            puts("mpv decoded the fixture to EOF");
            return;
        }
    }
    fprintf(stderr, "mpv playback timed out\n");
    exit(1);
}

static void remux(const char *source, const char *destination, const char *format)
{
    AVFormatContext *input = NULL, *output = NULL;
    check(avformat_open_input(&input, source, NULL, NULL), "open input");
    check(avformat_find_stream_info(input, NULL), "read streams");
    check(avformat_alloc_output_context2(&output, NULL, format, destination), "create output");
    if (!output) exit(1);
    int *mapping = av_malloc_array(input->nb_streams, sizeof(*mapping));
    if (!mapping) exit(1);
    int count = 0;
    for (unsigned int i = 0; i < input->nb_streams; i++) {
        AVStream *stream = input->streams[i];
        if (stream->codecpar->codec_type != AVMEDIA_TYPE_VIDEO &&
            stream->codecpar->codec_type != AVMEDIA_TYPE_AUDIO) {
            mapping[i] = -1;
            continue;
        }
        mapping[i] = count++;
        AVStream *copy = avformat_new_stream(output, NULL);
        if (!copy) exit(1);
        check(avcodec_parameters_copy(copy->codecpar, stream->codecpar), "copy codec parameters");
        copy->codecpar->codec_tag = 0;
        copy->time_base = stream->time_base;
    }
    output->avoid_negative_ts = AVFMT_AVOID_NEG_TS_MAKE_ZERO;
    if (!(output->oformat->flags & AVFMT_NOFILE))
        check(avio_open(&output->pb, destination, AVIO_FLAG_WRITE), "open output");
    AVDictionary *options = NULL;
    if (!strcmp(format, "hls")) {
        av_dict_set(&options, "hls_segment_type", "fmp4", 0);
        av_dict_set(&options, "hls_time", "1", 0);
        av_dict_set(&options, "hls_playlist_type", "vod", 0);
    }
    check(avformat_write_header(output, &options), "write header");
    av_dict_free(&options);
    AVPacket *packet = av_packet_alloc();
    if (!packet) exit(1);
    int status, packets = 0;
    while ((status = av_read_frame(input, packet)) >= 0) {
        int from = packet->stream_index, to = mapping[from];
        if (to >= 0) {
            /* A production remuxer must reconstruct missing decode timestamps
             * (e.g. initial B-frames in Matroska). This probe copies known
             * timestamps and must not silently ask the muxer to guess them. */
            if (packet->dts == AV_NOPTS_VALUE || packet->pts == AV_NOPTS_VALUE) {
                fprintf(stderr, "probe requires timestamped packets; input needs timestamp reconstruction\n");
                exit(1);
            }
            av_packet_rescale_ts(packet, input->streams[from]->time_base, output->streams[to]->time_base);
            packet->stream_index = to;
            packet->pos = -1;
            check(av_interleaved_write_frame(output, packet), "write packet");
            packets++;
        }
        av_packet_unref(packet);
    }
    if (status != AVERROR_EOF) check(status, "read packet");
    check(av_write_trailer(output), "write trailer");
    av_packet_free(&packet);
    av_free(mapping);
    avformat_close_input(&input);
    if (!(output->oformat->flags & AVFMT_NOFILE)) check(avio_closep(&output->pb), "close output");
    avformat_free_context(output);
    if (!packets) exit(1);
    printf("remuxed %d packets to %s (%s), without encoding\n", packets, destination, format);
}

int main(int argc, char **argv)
{
    capabilities();
    if (argc != 4) {
        fprintf(stderr, "usage: %s INPUT OUTPUT FORMAT\n", argv[0]);
        return 2;
    }
    playback(argv[1]);
    remux(argv[1], argv[2], argv[3]);
    return 0;
}
