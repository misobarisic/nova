/* Compiled against each target's own headers; no FFmpeg struct layout crosses FFI. */
#include <libavformat/avformat.h>
#include <libavcodec/bsf.h>
#include <libavcodec/version.h>
#include <libavutil/avutil.h>
#include <libavutil/mathematics.h>
#include <libavutil/time.h>
#include <libavutil/channel_layout.h>
#include <stdint.h>
#include <string.h>
#include <errno.h>

/* Ubuntu 22.04 packages FFmpeg 4.4; bundles use newer FFmpeg. Keep all
 * version-dependent structures behind helpers compiled against target headers. */
#if LIBAVCODEC_VERSION_INT >= AV_VERSION_INT(59, 24, 100)
static int channel_count(const AVCodecParameters *p) { return p->ch_layout.nb_channels; }
static void clear_channels(AVCodecParameters *p) { av_channel_layout_uninit(&p->ch_layout); }
static void default_channels(AVCodecParameters *p, int channels) {
    clear_channels(p); av_channel_layout_default(&p->ch_layout, channels);
}
#else
static int channel_count(const AVCodecParameters *p) { return p->channels; }
static void clear_channels(AVCodecParameters *p) { p->channels = 0; p->channel_layout = 0; }
static void default_channels(AVCodecParameters *p, int channels) {
    p->channels = channels; p->channel_layout = av_get_default_channel_layout(channels);
}
#endif
/* Older headers document zero as disabled but do not name the constant. */
#ifndef AVFMT_AVOID_NEG_TS_DISABLED
#define AVFMT_AVOID_NEG_TS_DISABLED 0
#endif
/* AVIO's write buffer became const in libavformat 61. */
#if LIBAVFORMAT_VERSION_MAJOR >= 61
#define NOVA_WRITE_CONST const
#else
#define NOVA_WRITE_CONST
#endif

typedef int (*Cancel)(void *);
typedef int (*Read)(void *, uint8_t *, int);
typedef int64_t (*Seek)(void *, int64_t, int);
typedef struct { Cancel cancel; void *opaque; int64_t limit; Read read; Seek seek; void *io; AVIOContext *avio; int64_t started; } Job;
static int interrupted(void *opaque);
typedef int (*Write)(void *, NOVA_WRITE_CONST uint8_t *, int);
typedef struct { void *opaque; Write write; Seek seek; int64_t position; Job *job; } Budget;
static int budget_write(void *opaque, NOVA_WRITE_CONST uint8_t *buffer, int size) {
    Budget *budget=opaque;
    if (interrupted(budget->job)) return AVERROR_EXIT;
    if (budget->position > budget->job->limit-size) return AVERROR(ENOSPC);
    int result=budget->write(budget->opaque,buffer,size);
    if(result>0)budget->position+=result;
    return result;
}
static int64_t budget_seek(void *opaque, int64_t offset, int whence) {
    Budget *budget=opaque;
    if(interrupted(budget->job))return AVERROR_EXIT;
    int64_t result=budget->seek(budget->opaque,offset,whence);
    if(result>=0 && !(whence&AVSEEK_SIZE))budget->position=result;
    return result;
}
typedef struct { int video; int audio; int container; double duration; int seekable; int width; int height; int audio_stream; int default_audio_stream; int audio_stream_count; } Probe;
static int interrupted(void *opaque) { Job *j = opaque; return j->cancel(j->opaque) || (j->started && av_gettime_relative()-j->started > 300LL*AV_TIME_BASE); }
/* Stable private IDs, deliberately unrelated to FFmpeg ABI enum values. */
static int codec_id(enum AVCodecID c) {
    switch(c) { case AV_CODEC_ID_H264: return 1; case AV_CODEC_ID_HEVC: return 2;
    case AV_CODEC_ID_VP8: return 3; case AV_CODEC_ID_VP9: return 4; case AV_CODEC_ID_AV1: return 5;
    case AV_CODEC_ID_AAC: return 10; case AV_CODEC_ID_MP3: return 11; case AV_CODEC_ID_OPUS: return 12;
    case AV_CODEC_ID_VORBIS: return 13; case AV_CODEC_ID_AC3: return 14; case AV_CODEC_ID_EAC3: return 15;
    default: return -1; }
}
/* Codec configuration parsing only: AAC-LC frame size is signalled in ASC. */
static void complete_parameters(AVFormatContext *in) {
    for(unsigned i=0;i<in->nb_streams;i++){
        AVCodecParameters *p=in->streams[i]->codecpar;
        if(p->codec_id==AV_CODEC_ID_AAC && !p->frame_size && p->extradata_size>=2 && (p->extradata[0]>>3)==2){
            int frequency=((p->extradata[0]&7)<<1)|(p->extradata[1]>>7);
            if(frequency!=15)p->frame_size=(p->extradata[1]&4)?960:1024;
        }
    }
}
static int open_input(const char *path, Job *job, AVFormatContext **out) {
    AVFormatContext *in = avformat_alloc_context();
    if (!in) return AVERROR(ENOMEM);
    in->interrupt_callback.callback = interrupted; in->interrupt_callback.opaque = job;
    if (job->read) {
        uint8_t *buffer = av_malloc(32768);
        if (!buffer) { avformat_free_context(in); return AVERROR(ENOMEM); }
        job->avio = avio_alloc_context(buffer,32768,0,job->io,job->read,NULL,job->seek);
        if (!job->avio) { av_free(buffer); avformat_free_context(in); return AVERROR(ENOMEM); }
        in->pb = job->avio; in->flags |= AVFMT_FLAG_CUSTOM_IO;
    }
    AVDictionary *opts = NULL;
    /* Custom AVIO owns source authorization. Forbid nested network references and all decoder opens. */
    av_dict_set(&opts, "codec_whitelist", "nova_packet_copy_no_decoders", 0);
    av_dict_set(&opts, "protocol_whitelist", "file", 0);
    av_dict_set(&opts, "format_whitelist", "mov,mp4,m4a,3gp,3g2,mj2,matroska,webm,mpegts,aac,mp3,ogg,flac,avi", 0);
    av_dict_set(&opts, "probesize", "8388608", 0);
    av_dict_set(&opts, "max_streams", "64", 0);
    av_dict_set(&opts, "enable_drefs", "0", 0);
    av_dict_set(&opts, "use_absolute_path", "0", 0);
    av_dict_set(&opts, "analyzeduration", "5000000", 0);
    int ret = avformat_open_input(&in, path, NULL, &opts); av_dict_free(&opts);
    if (ret < 0) { avformat_free_context(in); return ret; }
    complete_parameters(in);
    ret = avformat_find_stream_info(in, NULL);
    complete_parameters(in);
    if (ret < 0) { avformat_close_input(&in); return ret; }
    *out = in; return 0;
}
static int default_audio(AVFormatContext *in){
    int first=-1;for(unsigned i=0;i<in->nb_streams;i++){AVStream *s=in->streams[i];if(s->codecpar->codec_type==AVMEDIA_TYPE_AUDIO){if(first<0)first=i;if(s->disposition&AV_DISPOSITION_DEFAULT)return i;}}return first;
}
int nova_media_probe(const char *path, Cancel cancel, void *opaque, Probe *probe, Read read, Seek seek, void *io, int selected_audio_index) {
    Job job = {cancel, opaque, 0, read, seek, io, NULL, av_gettime_relative()}; AVFormatContext *in = NULL;
    int ret = open_input(path, &job, &in); if (ret < 0) { if(job.avio) {av_freep(&job.avio->buffer);avio_context_free(&job.avio);} return ret; }
    memset(probe, 0, sizeof(*probe));probe->audio_stream=-1;probe->default_audio_stream=default_audio(in);
    if(selected_audio_index<0)selected_audio_index=probe->default_audio_stream;
    const char *name = in->iformat->name;
    if (strstr(name,"mov") || strstr(name,"mp4")) probe->container = 1;
    else if (strstr(name,"webm") || strstr(name,"matroska")) {
        probe->container=3;
        /* The shared Matroska/WebM demuxer name cannot identify the EBML DocType. */
        int64_t current=avio_tell(in->pb);uint8_t header[1024];
        if(avio_seek(in->pb,0,SEEK_SET)>=0){int n=avio_read(in->pb,header,sizeof(header));for(int i=0;i+7<=n;i++)if(!memcmp(header+i,"\x42\x82\x84webm",7)){probe->container=2;break;}avio_seek(in->pb,current,SEEK_SET);}
    }
    else if (strstr(name,"mpegts")) probe->container = 4;
    else if (strstr(name,"mp3")) probe->container = 5;
    else if (strstr(name,"ogg")) probe->container = 6;
    else probe->container = -1;
    for (unsigned i=0; i<in->nb_streams; i++) {
        AVCodecParameters *p = in->streams[i]->codecpar;
        if(p->codec_type==AVMEDIA_TYPE_AUDIO)probe->audio_stream_count++;
        if (p->codec_type == AVMEDIA_TYPE_VIDEO && !(in->streams[i]->disposition & AV_DISPOSITION_ATTACHED_PIC) && !probe->video) {
            probe->video = codec_id(p->codec_id); probe->width = p->width; probe->height = p->height;
        } else if (p->codec_type == AVMEDIA_TYPE_AUDIO && !probe->audio && (selected_audio_index<0 || i==(unsigned)selected_audio_index)) {probe->audio = codec_id(p->codec_id);probe->audio_stream=i;}
    }
    probe->duration = in->duration == AV_NOPTS_VALUE ? -1.0 : (double)in->duration / AV_TIME_BASE;
    probe->seekable = in->pb && (in->pb->seekable & AVIO_SEEKABLE_NORMAL);
    if(selected_audio_index>=0 && probe->audio_stream<0)ret=AVERROR_STREAM_NOT_FOUND;
    avformat_close_input(&in); if(job.avio) {av_freep(&job.avio->buffer);avio_context_free(&job.avio);} return ret;
}
/* An ADTS header carries all metadata required to copy AAC-LC, without decoding. */
static int adts_parameters(AVCodecParameters *p,const AVPacket *packet) {
    const uint8_t *d=packet->data;
    static const int rates[]={96000,88200,64000,48000,44100,32000,24000,22050,16000,12000,11025,8000,7350};
    if(packet->size<7||d[0]!=0xff||(d[1]&0xf6)!=0xf0||(d[2]>>6)!=1||(d[6]&3))return AVERROR_INVALIDDATA;
    int rate=(d[2]>>2)&15,channels=((d[2]&1)<<2)|(d[3]>>6);
    if(rate>=13||channels==0)return AVERROR_INVALIDDATA;
    p->sample_rate=rates[rate];p->frame_size=1024;
    default_channels(p,channels==7?8:channels);
    return 0;
}
static int extract_parameters(AVCodecParameters *p,const AVPacket *packet) {
    const AVBitStreamFilter *filter=av_bsf_get_by_name("extract_extradata");
    if(!filter)return AVERROR(ENOSYS);
    AVBSFContext *bsf=NULL;AVPacket *copy=NULL;int ret=av_bsf_alloc(filter,&bsf);if(ret<0)return ret;
    ret=avcodec_parameters_copy(bsf->par_in,p);if(ret<0)goto end;
    ret=av_bsf_init(bsf);if(ret<0)goto end;
    copy=av_packet_clone(packet);if(!copy){ret=AVERROR(ENOMEM);goto end;}
    ret=av_bsf_send_packet(bsf,copy);if(ret<0)goto end;
    ret=av_bsf_receive_packet(bsf,copy);if(ret==AVERROR(EAGAIN)||ret==AVERROR_EOF){ret=0;goto end;}if(ret<0)goto end;
    #if LIBAVCODEC_VERSION_MAJOR >= 59
    size_t size=0;
#else
    int size=0;
#endif
    const uint8_t *extra=av_packet_get_side_data(copy,AV_PKT_DATA_NEW_EXTRADATA,&size);
    if(extra&&size&&size<=8388608){
        uint8_t *data=av_mallocz(size+AV_INPUT_BUFFER_PADDING_SIZE);if(!data){ret=AVERROR(ENOMEM);goto end;}
        memcpy(data,extra,size);av_freep(&p->extradata);p->extradata=data;p->extradata_size=size;
    }
end:av_packet_free(&copy);av_bsf_free(&bsf);return ret;
}
int nova_media_remux(const char *source, const char *destination, Cancel cancel, void *opaque, int64_t limit, Read read, Seek seek, void *io, int selected_audio_index) {
    Job job = {cancel,opaque,limit,read,seek,io,NULL,av_gettime_relative()}; AVFormatContext *in = NULL, *out = NULL;
    AVPacket *packet = NULL; AVBSFContext **filters = NULL; int *map = NULL; Budget budget={0}; AVIOContext *backing=NULL;
    AVPacket *prelude[256]={0};int prelude_count=0,prelude_cursor=0;
    int64_t *last_dts = NULL,*audio_anchor=NULL,*audio_samples=NULL; int ret = open_input(source, &job, &in), opened = 0;
    if (ret < 0) goto cleanup;
    int audio_index=selected_audio_index;
    if(audio_index<0)audio_index=default_audio(in);
    AVCodecParameters *audio=audio_index>=0 && (unsigned)audio_index<in->nb_streams?in->streams[audio_index]->codecpar:NULL;
    int video_index=-1;for(unsigned i=0;i<in->nb_streams;i++)if(in->streams[i]->codecpar->codec_type==AVMEDIA_TYPE_VIDEO&&!(in->streams[i]->disposition&AV_DISPOSITION_ATTACHED_PIC)){video_index=i;break;}
    AVCodecParameters *video=video_index>=0?in->streams[video_index]->codecpar:NULL;
    int64_t prelude_bytes=0;
    while((audio&&audio->codec_id==AV_CODEC_ID_AAC&&(!audio->sample_rate||!channel_count(audio))) || (video&&(video->codec_id==AV_CODEC_ID_H264||video->codec_id==AV_CODEC_ID_HEVC)&&!video->extradata_size)){
        if(prelude_count==256||prelude_bytes>8388608||interrupted(&job)){ret=AVERROR_INVALIDDATA;goto cleanup;}
        AVPacket *p=av_packet_alloc();if(!p){ret=AVERROR(ENOMEM);goto cleanup;}
        ret=av_read_frame(in,p);if(ret<0){av_packet_free(&p);goto cleanup;}
        prelude[prelude_count++]=p;prelude_bytes+=p->size;
        if(p->stream_index==audio_index&&audio&&audio->codec_id==AV_CODEC_ID_AAC&&(!audio->sample_rate||!channel_count(audio))){ret=adts_parameters(audio,p);if(ret<0){goto cleanup;}}
        if(p->stream_index==video_index&&video&&!video->extradata_size){ret=extract_parameters(video,p);if(ret<0){goto cleanup;}}
    }
    ret = avformat_alloc_output_context2(&out,NULL,"mp4",destination); if (ret < 0 || !out) goto cleanup;
    out->interrupt_callback.callback = interrupted; out->interrupt_callback.opaque = &job;
    map = av_malloc_array(in->nb_streams,sizeof(*map));
    filters = av_calloc(in->nb_streams,sizeof(*filters));
    last_dts = av_malloc_array(in->nb_streams,sizeof(*last_dts));
    audio_anchor=av_malloc_array(in->nb_streams,sizeof(*audio_anchor));audio_samples=av_calloc(in->nb_streams,sizeof(*audio_samples));
    if (!map || !filters || !last_dts || !audio_anchor || !audio_samples) { ret = AVERROR(ENOMEM); goto cleanup; }
    int selected_video = 0, selected_audio = 0;
    for(unsigned i=0;i<in->nb_streams;i++) {
        AVStream *s = in->streams[i]; AVCodecParameters *p = s->codecpar; map[i] = -1; last_dts[i] = AV_NOPTS_VALUE;audio_anchor[i]=AV_NOPTS_VALUE;
        if (p->codec_type == AVMEDIA_TYPE_VIDEO && !(s->disposition & AV_DISPOSITION_ATTACHED_PIC) && !selected_video) selected_video=1;
        else if (p->codec_type == AVMEDIA_TYPE_AUDIO && !selected_audio && (audio_index<0 || i==(unsigned)audio_index)) selected_audio=1;
        else continue;
        if (p->codec_id != AV_CODEC_ID_H264 && p->codec_id != AV_CODEC_ID_HEVC && p->codec_id != AV_CODEC_ID_AV1 && p->codec_id != AV_CODEC_ID_AAC && p->codec_id != AV_CODEC_ID_MP3) {ret=AVERROR(ENOSYS);goto cleanup;}
        AVStream *d = avformat_new_stream(out,NULL); if (!d) {ret=AVERROR(ENOMEM);goto cleanup;}
        map[i] = d->index;
        const char *filter = NULL;
        if (p->codec_id == AV_CODEC_ID_AAC && !p->extradata_size) filter = "aac_adtstoasc";
        /* Annex B AVC/HEVC needs decoder configuration extracted before MP4 header. */
        if ((p->codec_id == AV_CODEC_ID_H264 || p->codec_id == AV_CODEC_ID_HEVC) && !p->extradata_size) {ret=AVERROR_INVALIDDATA;goto cleanup;}
        if (filter) {
            const AVBitStreamFilter *bsf = av_bsf_get_by_name(filter); if (!bsf) {ret=AVERROR(ENOSYS);goto cleanup;}
            ret=av_bsf_alloc(bsf,&filters[i]); if(ret<0)goto cleanup;
            ret=avcodec_parameters_copy(filters[i]->par_in,p); if(ret<0)goto cleanup;
            filters[i]->time_base_in=s->time_base;
            ret=av_bsf_init(filters[i]); if(ret<0)goto cleanup;
            ret=avcodec_parameters_copy(d->codecpar,filters[i]->par_out);
        } else ret=avcodec_parameters_copy(d->codecpar,p);
        if(ret<0)goto cleanup;
        d->codecpar->codec_tag=0; d->time_base=s->time_base;
        if (p->codec_id==AV_CODEC_ID_HEVC) d->codecpar->codec_tag=MKTAG('h','v','c','1');
    }
    if(selected_audio_index>=0 && !selected_audio){ret=AVERROR_STREAM_NOT_FOUND;goto cleanup;}
    if(!selected_video && !selected_audio) {ret=AVERROR_STREAM_NOT_FOUND;goto cleanup;}
    ret=avio_open(&backing,destination,AVIO_FLAG_WRITE); if(ret<0)goto cleanup; opened=1;
    budget=(Budget){backing->opaque,backing->write_packet,backing->seek,0,&job};
    uint8_t *write_buffer=av_malloc(32768);
    if(!write_buffer){ret=AVERROR(ENOMEM);goto cleanup;}
    out->pb=avio_alloc_context(write_buffer,32768,1,&budget,NULL,budget_write,budget_seek);
    if(!out->pb){av_free(write_buffer);ret=AVERROR(ENOMEM);goto cleanup;}
    out->flags|=AVFMT_FLAG_CUSTOM_IO;
    AVDictionary *opts=NULL; av_dict_set(&opts,"movflags","+faststart",0);
    out->avoid_negative_ts=AVFMT_AVOID_NEG_TS_MAKE_NON_NEGATIVE;
    ret=avformat_write_header(out,&opts); av_dict_free(&opts); if(ret<0)goto cleanup;
    packet=av_packet_alloc(); if(!packet){ret=AVERROR(ENOMEM);goto cleanup;}
    while(1) {
        if(prelude_cursor<prelude_count){av_packet_move_ref(packet,prelude[prelude_cursor++]);ret=0;}else ret=av_read_frame(in,packet);
        if(ret<0)break;
        if(interrupted(&job)){ret=AVERROR_EXIT;goto cleanup;}
        unsigned idx=packet->stream_index;
        if(map[idx]<0){av_packet_unref(packet);continue;}
        AVStream *s=in->streams[idx],*d=out->streams[map[idx]];

        if(d->codecpar->codec_id==AV_CODEC_ID_AAC && filters[idx]){
            AVCodecParameters configuration={0};ret=adts_parameters(&configuration,packet);
            int matches=ret>=0&&configuration.sample_rate==d->codecpar->sample_rate&&channel_count(&configuration)==channel_count(d->codecpar);
            clear_channels(&configuration);
            if(!matches){ret=AVERROR_INVALIDDATA;goto cleanup;}
        }
        if(d->codecpar->codec_id==AV_CODEC_ID_AAC && packet->duration<=0 && d->codecpar->frame_size<=0){ret=AVERROR_INVALIDDATA;goto cleanup;}
        /* AAC has no reorder delay. Missing timestamps after a known anchor can
         * be recovered from its signalled sample count without drift. Video DTS
         * is never synthesized, especially for ambiguous Matroska B frames. */
        if(d->codecpar->codec_id==AV_CODEC_ID_AAC && d->codecpar->sample_rate>0 && d->codecpar->frame_size>0){
            AVRational samples={1,d->codecpar->sample_rate};
            if(packet->dts==AV_NOPTS_VALUE&&packet->pts!=AV_NOPTS_VALUE)packet->dts=packet->pts;
            if(packet->dts!=AV_NOPTS_VALUE){audio_anchor[idx]=packet->dts;audio_samples[idx]=0;}
            else if(audio_anchor[idx]!=AV_NOPTS_VALUE)packet->dts=audio_anchor[idx]+av_rescale_q(audio_samples[idx],samples,s->time_base);
            if(packet->pts==AV_NOPTS_VALUE)packet->pts=packet->dts;
            if(packet->duration<=0)packet->duration=av_rescale_q(audio_samples[idx]+d->codecpar->frame_size,samples,s->time_base)-av_rescale_q(audio_samples[idx],samples,s->time_base);
            audio_samples[idx]+=d->codecpar->frame_size;
        }
        if(packet->dts==AV_NOPTS_VALUE || packet->pts==AV_NOPTS_VALUE || (last_dts[idx]!=AV_NOPTS_VALUE && packet->dts<=last_dts[idx])) {ret=AVERROR_INVALIDDATA;goto cleanup;}
        last_dts[idx]=packet->dts;
        if(filters[idx]) {
            ret=av_bsf_send_packet(filters[idx],packet); if(ret<0){goto cleanup;}
            while((ret=av_bsf_receive_packet(filters[idx],packet))>=0) {
                av_packet_rescale_ts(packet,filters[idx]->time_base_out,d->time_base);packet->stream_index=d->index;packet->pos=-1;
                ret=av_interleaved_write_frame(out,packet);if(ret<0)goto cleanup;
            }
            if(ret!=AVERROR(EAGAIN)&&ret!=AVERROR_EOF){goto cleanup;}
        } else {
            av_packet_rescale_ts(packet,s->time_base,d->time_base);packet->stream_index=d->index;packet->pos=-1;
            ret=av_interleaved_write_frame(out,packet);if(ret<0)goto cleanup;
        }
        if(avio_tell(out->pb)>limit){ret=AVERROR(ENOSPC);goto cleanup;}
    }
    if(ret!=AVERROR_EOF)goto cleanup;
    for(unsigned i=0;i<in->nb_streams;i++) if(filters[i]) {
        ret=av_bsf_send_packet(filters[i],NULL);if(ret<0)goto cleanup;
        while((ret=av_bsf_receive_packet(filters[i],packet))>=0){AVStream *d=out->streams[map[i]];av_packet_rescale_ts(packet,filters[i]->time_base_out,d->time_base);packet->stream_index=d->index;packet->pos=-1;ret=av_interleaved_write_frame(out,packet);if(ret<0){goto cleanup;}}
        if(ret!=AVERROR_EOF&&ret!=AVERROR(EAGAIN))goto cleanup;
    }
    ret=av_write_trailer(out);
    if(ret>=0 && avio_tell(out->pb)>limit)ret=AVERROR(ENOSPC);
cleanup:
    for(int i=0;i<prelude_count;i++)av_packet_free(&prelude[i]);
    av_packet_free(&packet);
    if(filters&&in){for(unsigned i=0;i<in->nb_streams;i++)av_bsf_free(&filters[i]);}
    av_free(filters);av_free(map);av_free(last_dts);av_free(audio_anchor);av_free(audio_samples);
    if(opened){
        if(out->pb){avio_flush(out->pb);if(ret>=0&&out->pb->error<0)ret=out->pb->error;av_freep(&out->pb->buffer);avio_context_free(&out->pb);}
        int close_ret=avio_closep(&backing);if(ret>=0&&close_ret<0)ret=close_ret;
    }
    avformat_free_context(out);avformat_close_input(&in);
    if(job.avio) {av_freep(&job.avio->buffer);avio_context_free(&job.avio);}
    return ret;
}
/* Regression-only packet identity helper; never opens a codec context. */
int nova_media_packet_fingerprint(const char *path, uint64_t *video, uint64_t *audio, int *packets, int selected_audio) {
    AVFormatContext *in=NULL;AVPacket *packet=NULL;int ret=avformat_open_input(&in,path,NULL,NULL);
    if(ret<0)return ret;
    ret=avformat_find_stream_info(in,NULL);if(ret<0)goto end;
    *video=1469598103934665603ULL;*audio=1469598103934665603ULL;*packets=0;
    packet=av_packet_alloc();if(!packet){ret=AVERROR(ENOMEM);goto end;}
    while((ret=av_read_frame(in,packet))>=0){
        enum AVMediaType type=in->streams[packet->stream_index]->codecpar->codec_type;
        uint64_t *hash=type==AVMEDIA_TYPE_VIDEO?video:type==AVMEDIA_TYPE_AUDIO && (selected_audio<0 || packet->stream_index==selected_audio)?audio:NULL;
        if(hash){for(int i=0;i<packet->size;i++){*hash^=packet->data[i];*hash*=1099511628211ULL;}(*packets)++;}
        av_packet_unref(packet);
    }
    if(ret==AVERROR_EOF)ret=0;
end:av_packet_free(&packet);avformat_close_input(&in);return ret;
}

#if defined(_WIN32)
#include <windows.h>
int64_t nova_media_available_space(const char *path) {
    int count=MultiByteToWideChar(CP_UTF8,MB_ERR_INVALID_CHARS,path,-1,NULL,0);if(!count)return -1;
    wchar_t *wide=av_malloc_array(count,sizeof(*wide));if(!wide)return -1;
    if(!MultiByteToWideChar(CP_UTF8,MB_ERR_INVALID_CHARS,path,-1,wide,count)){av_free(wide);return -1;}
    ULARGE_INTEGER available;int result=GetDiskFreeSpaceExW(wide,&available,NULL,NULL);av_free(wide);
    return result?(available.QuadPart>INT64_MAX?INT64_MAX:(int64_t)available.QuadPart):-1;
}
#else
#include <sys/statvfs.h>
int64_t nova_media_available_space(const char *path) {
    struct statvfs status;if(statvfs(path,&status)<0)return -1;
    uint64_t size=status.f_frsize?status.f_frsize:status.f_bsize;
    if(size && status.f_bavail>(uint64_t)INT64_MAX/size)return INT64_MAX;
    return status.f_bavail*size;
}
#endif

#include "stream_copy.c"
/* Regression timing snapshot, in demux order; test fixture callers cap packets. */
int nova_media_packet_times(const char *path, double *times, int capacity) {
    AVFormatContext *in=NULL;AVPacket *p=NULL;int count=0,ret=avformat_open_input(&in,path,NULL,NULL);
    if(ret<0)return ret;
    p=av_packet_alloc();if(!p){avformat_close_input(&in);return AVERROR(ENOMEM);}
    while(count<capacity&&(ret=av_read_frame(in,p))>=0){
        AVStream *s=in->streams[p->stream_index];
        if(s->codecpar->codec_type==AVMEDIA_TYPE_VIDEO||s->codecpar->codec_type==AVMEDIA_TYPE_AUDIO){
            times[count*3]=s->codecpar->codec_type==AVMEDIA_TYPE_VIDEO?0:1;
            times[count*3+1]=p->pts*av_q2d(s->time_base);times[count*3+2]=p->dts*av_q2d(s->time_base);count++;
        }
        av_packet_unref(p);
    }
    av_packet_free(&p);avformat_close_input(&in);return count;
}
